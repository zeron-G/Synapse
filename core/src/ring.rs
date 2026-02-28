//! Lock-free SPSC ring buffer with cacheline-aligned head/tail.

use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cacheline size (64 bytes on x86_64 / ARM).
const CACHELINE: usize = 64;

/// Ring buffer metadata header, placed at the start of each ring region.
///
/// Layout:
///   [0..64)   head (cacheline 0)
///   [64..128) tail (cacheline 1)
///   [128..192) metadata (cacheline 2)
///   [192..)   slots data
#[repr(C)]
pub struct RingHeader {
    /// Write position (owned by producer). Cacheline-aligned.
    pub head: AtomicU64,
    pub _pad_head: [u8; CACHELINE - 8],

    /// Read position (owned by consumer). Cacheline-aligned.
    pub tail: AtomicU64,
    pub _pad_tail: [u8; CACHELINE - 8],

    /// Number of slots (must be power of 2).
    pub capacity: u64,
    /// Size of each slot in bytes (includes 4-byte length prefix).
    pub slot_size: u64,
    /// Bitmask: capacity - 1.
    pub mask: u64,
    pub _pad_meta: [u8; CACHELINE - 24],
}

/// Size of the ring header (3 cachelines = 192 bytes).
pub const RING_HEADER_SIZE: usize = std::mem::size_of::<RingHeader>();

impl RingHeader {
    /// Initialize a ring header at the given pointer.
    ///
    /// # Safety
    /// `ptr` must point to zeroed, 64-byte aligned memory of at least
    /// `Self::region_size(capacity, slot_size)`.
    pub unsafe fn init(ptr: *mut u8, capacity: u64, slot_size: u64) {
        assert!(capacity.is_power_of_two(), "capacity must be power of 2");
        assert!(slot_size >= 8, "slot_size must be at least 8 bytes");

        // Write fields manually to avoid alignment issues with the struct
        let p = ptr;
        // head at offset 0
        (p as *mut u64).write(0);
        // tail at offset 64
        (p.add(CACHELINE) as *mut u64).write(0);
        // capacity at offset 128
        (p.add(2 * CACHELINE) as *mut u64).write(capacity);
        // slot_size at offset 128 + 8
        (p.add(2 * CACHELINE + 8) as *mut u64).write(slot_size);
        // mask at offset 128 + 16
        (p.add(2 * CACHELINE + 16) as *mut u64).write(capacity - 1);
    }

    /// Total bytes needed for this ring (header + all slots).
    pub fn region_size(capacity: u64, slot_size: u64) -> usize {
        RING_HEADER_SIZE + (capacity as usize) * (slot_size as usize)
    }

    /// Read capacity from the header (avoids alignment issues).
    #[inline]
    fn get_capacity(base: *const u8) -> u64 {
        unsafe { (base.add(2 * CACHELINE) as *const u64).read() }
    }

    #[inline]
    fn get_slot_size(base: *const u8) -> u64 {
        unsafe { (base.add(2 * CACHELINE + 8) as *const u64).read() }
    }

    #[inline]
    fn get_mask(base: *const u8) -> u64 {
        unsafe { (base.add(2 * CACHELINE + 16) as *const u64).read() }
    }

    #[inline]
    fn head_atomic(base: *const u8) -> &'static AtomicU64 {
        unsafe { &*(base as *const AtomicU64) }
    }

    #[inline]
    fn tail_atomic(base: *const u8) -> &'static AtomicU64 {
        unsafe { &*(base.add(CACHELINE) as *const AtomicU64) }
    }

    /// Get a pointer to the slot at the given index.
    #[inline]
    unsafe fn slot_ptr_raw(base: *const u8, index: u64, slot_size: u64) -> *mut u8 {
        let slots_base = base.add(RING_HEADER_SIZE);
        slots_base.add((index * slot_size) as usize) as *mut u8
    }
}

/// Safe wrapper for ring buffer operations using raw pointer arithmetic.
/// This avoids alignment issues with `#[repr(C, align(64))]` on non-aligned buffers.
pub struct Ring {
    base: *mut u8,
}

unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    /// Create a Ring view over the given base pointer.
    ///
    /// # Safety
    /// `base` must point to a properly initialized ring header region.
    pub unsafe fn from_ptr(base: *mut u8) -> Self {
        Self { base }
    }

    fn capacity(&self) -> u64 {
        RingHeader::get_capacity(self.base)
    }

    fn slot_size(&self) -> u64 {
        RingHeader::get_slot_size(self.base)
    }

    fn mask(&self) -> u64 {
        RingHeader::get_mask(self.base)
    }

    fn head(&self) -> &AtomicU64 {
        RingHeader::head_atomic(self.base)
    }

    fn tail(&self) -> &AtomicU64 {
        RingHeader::tail_atomic(self.base)
    }

    /// Try to push data into the ring (producer side).
    pub fn try_push(&self, data: &[u8]) -> crate::error::Result<()> {
        let slot_size = self.slot_size();
        let max_payload = slot_size as usize - 4;
        if data.len() > max_payload {
            return Err(crate::error::SynapseError::DataTooLarge {
                data_len: data.len(),
                slot_size: max_payload,
            });
        }

        let head = self.head().load(Ordering::Relaxed);
        let tail = self.tail().load(Ordering::Acquire);

        if head.wrapping_sub(tail) >= self.capacity() {
            return Err(crate::error::SynapseError::RingFull);
        }

        unsafe {
            let slot = RingHeader::slot_ptr_raw(self.base, head & self.mask(), slot_size);
            ptr::write(slot as *mut u32, data.len() as u32);
            ptr::copy_nonoverlapping(data.as_ptr(), slot.add(4), data.len());
        }

        self.head().store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Try to pop data from the ring (consumer side).
    pub fn try_pop(&self, buf: &mut [u8]) -> crate::error::Result<usize> {
        let tail = self.tail().load(Ordering::Relaxed);
        let head = self.head().load(Ordering::Acquire);

        if head == tail {
            return Err(crate::error::SynapseError::RingEmpty);
        }

        let slot_size = self.slot_size();
        let len;
        unsafe {
            let slot = RingHeader::slot_ptr_raw(self.base, tail & self.mask(), slot_size);
            len = ptr::read(slot as *const u32) as usize;
            if len > buf.len() {
                return Err(crate::error::SynapseError::DataTooLarge {
                    data_len: len,
                    slot_size: buf.len(),
                });
            }
            ptr::copy_nonoverlapping(slot.add(4), buf.as_mut_ptr(), len);
        }

        self.tail().store(tail.wrapping_add(1), Ordering::Release);
        Ok(len)
    }

    /// Number of items currently in the ring.
    pub fn len(&self) -> u64 {
        let head = self.head().load(Ordering::Acquire);
        let tail = self.tail().load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Maximum payload size per slot (slot_size minus 4-byte length prefix).
    pub fn slot_payload_size(&self) -> usize {
        self.slot_size() as usize - 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc_ring(capacity: u64, slot_size: u64) -> (Vec<u8>, Ring) {
        let size = RingHeader::region_size(capacity, slot_size);
        let mut region = vec![0u8; size];
        unsafe {
            RingHeader::init(region.as_mut_ptr(), capacity, slot_size);
            let ring = Ring::from_ptr(region.as_mut_ptr());
            (region, ring)
        }
    }

    #[test]
    fn test_ring_push_pop() {
        let (_mem, ring) = alloc_ring(16, 64);
        assert!(ring.is_empty());

        ring.try_push(b"hello").unwrap();
        assert_eq!(ring.len(), 1);

        let mut buf = [0u8; 256];
        let len = ring.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"hello");
        assert!(ring.is_empty());
    }

    #[test]
    fn test_ring_full() {
        let (_mem, ring) = alloc_ring(4, 32);
        for i in 0..4 {
            ring.try_push(&[i as u8; 10]).unwrap();
        }
        assert_eq!(
            ring.try_push(b"overflow"),
            Err(crate::error::SynapseError::RingFull)
        );
    }

    #[test]
    fn test_ring_wraparound() {
        let (_mem, ring) = alloc_ring(4, 32);
        let mut buf = [0u8; 256];

        for round in 0..3u8 {
            for i in 0..4u8 {
                ring.try_push(&[round * 10 + i; 8]).unwrap();
            }
            for i in 0..4u8 {
                let len = ring.try_pop(&mut buf).unwrap();
                assert_eq!(len, 8);
                assert_eq!(buf[0], round * 10 + i);
            }
        }
    }

    #[test]
    fn test_slot_ptr_uses_correct_offset() {
        let (_mem, ring) = alloc_ring(4, 64);

        ring.try_push(b"AAAA").unwrap();
        ring.try_push(b"BBBB").unwrap();
        ring.try_push(b"CCCC").unwrap();

        let mut buf = [0u8; 64];
        let len = ring.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"AAAA");
        let len = ring.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"BBBB");
        let len = ring.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"CCCC");
    }
}
