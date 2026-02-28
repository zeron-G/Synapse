//! Lock-free SPSC ring buffer with cacheline-aligned head/tail.

use std::sync::atomic::{AtomicU64, Ordering};
use std::ptr;

/// Cacheline size (64 bytes on x86_64 / ARM).
const CACHELINE: usize = 64;

/// Ring buffer metadata header, placed at the start of each ring region.
///
/// Layout:
///   [0..64)   head (cacheline 0)
///   [64..128) tail (cacheline 1)
///   [128..)   slots data
#[repr(C, align(64))]
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

const _: () = {
    // head and tail must be on separate cachelines
    assert!(std::mem::offset_of!(RingHeader, head) == 0);
    assert!(std::mem::offset_of!(RingHeader, tail) == CACHELINE);
    // Metadata starts at cacheline 2
    assert!(std::mem::offset_of!(RingHeader, capacity) == 2 * CACHELINE);
};

/// Size of the ring header (3 cachelines = 192 bytes).
pub const RING_HEADER_SIZE: usize = std::mem::size_of::<RingHeader>();

impl RingHeader {
    /// Initialize a ring header at the given pointer.
    ///
    /// # Safety
    /// `ptr` must point to zeroed memory of at least `Self::region_size(capacity, slot_size)`.
    pub unsafe fn init(ptr: *mut u8, capacity: u64, slot_size: u64) {
        assert!(capacity.is_power_of_two(), "capacity must be power of 2");
        assert!(slot_size >= 8, "slot_size must be at least 8 bytes");
        let hdr = &mut *(ptr as *mut RingHeader);
        hdr.head = AtomicU64::new(0);
        hdr.tail = AtomicU64::new(0);
        hdr.capacity = capacity;
        hdr.slot_size = slot_size;
        hdr.mask = capacity - 1;
    }

    /// Total bytes needed for this ring (header + all slots).
    pub fn region_size(capacity: u64, slot_size: u64) -> usize {
        RING_HEADER_SIZE + (capacity as usize) * (slot_size as usize)
    }

    /// Get a pointer to the slot at the given index.
    ///
    /// # Safety
    /// `index` must be < capacity. The returned pointer is within the mapped region.
    #[inline]
    unsafe fn slot_ptr(&self, index: u64) -> *mut u8 {
        let base = (self as *const Self as *const u8).add(RING_HEADER_SIZE);
        base.add((index * self.slot_size) as usize) as *mut u8
    }

    /// Try to push data into the ring (producer side).
    ///
    /// Returns `Err(RingFull)` if no space available.
    /// Returns `Err(DataTooLarge)` if data exceeds slot capacity.
    pub fn try_push(&self, data: &[u8]) -> crate::error::Result<()> {
        let max_payload = self.slot_size as usize - 4; // 4 bytes for length prefix
        if data.len() > max_payload {
            return Err(crate::error::SynapseError::DataTooLarge {
                data_len: data.len(),
                slot_size: max_payload,
            });
        }

        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        if head.wrapping_sub(tail) >= self.capacity {
            return Err(crate::error::SynapseError::RingFull);
        }

        unsafe {
            let slot = self.slot_ptr(head & self.mask);
            // Write length prefix (u32 LE)
            ptr::write(slot as *mut u32, data.len() as u32);
            // Write payload
            ptr::copy_nonoverlapping(data.as_ptr(), slot.add(4), data.len());
        }

        self.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Try to pop data from the ring (consumer side).
    ///
    /// Returns `Ok(data)` with the message bytes, or `Err(RingEmpty)`.
    pub fn try_pop(&self, buf: &mut [u8]) -> crate::error::Result<usize> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if head == tail {
            return Err(crate::error::SynapseError::RingEmpty);
        }

        let len;
        unsafe {
            let slot = self.slot_ptr(tail & self.mask);
            len = ptr::read(slot as *const u32) as usize;
            if len > buf.len() {
                return Err(crate::error::SynapseError::DataTooLarge {
                    data_len: len,
                    slot_size: buf.len(),
                });
            }
            ptr::copy_nonoverlapping(slot.add(4), buf.as_mut_ptr(), len);
        }

        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(len)
    }

    /// Number of items currently in the ring.
    pub fn len(&self) -> u64 {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_push_pop() {
        let capacity: u64 = 16;
        let slot_size: u64 = 64;
        let size = RingHeader::region_size(capacity, slot_size);
        let mut region = vec![0u8; size];

        unsafe { RingHeader::init(region.as_mut_ptr(), capacity, slot_size); }
        let hdr = unsafe { &*(region.as_ptr() as *const RingHeader) };

        assert!(hdr.is_empty());

        // Push
        hdr.try_push(b"hello").unwrap();
        assert_eq!(hdr.len(), 1);

        // Pop
        let mut buf = [0u8; 256];
        let len = hdr.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"hello");
        assert!(hdr.is_empty());
    }

    #[test]
    fn test_ring_full() {
        let capacity: u64 = 4;
        let slot_size: u64 = 32;
        let size = RingHeader::region_size(capacity, slot_size);
        let mut region = vec![0u8; size];

        unsafe { RingHeader::init(region.as_mut_ptr(), capacity, slot_size); }
        let hdr = unsafe { &*(region.as_ptr() as *const RingHeader) };

        for i in 0..4 {
            hdr.try_push(&[i as u8; 10]).unwrap();
        }
        assert_eq!(hdr.try_push(b"overflow"), Err(crate::error::SynapseError::RingFull));
    }

    #[test]
    fn test_ring_wraparound() {
        let capacity: u64 = 4;
        let slot_size: u64 = 32;
        let size = RingHeader::region_size(capacity, slot_size);
        let mut region = vec![0u8; size];

        unsafe { RingHeader::init(region.as_mut_ptr(), capacity, slot_size); }
        let hdr = unsafe { &*(region.as_ptr() as *const RingHeader) };

        let mut buf = [0u8; 256];

        // Fill and drain multiple times to test wraparound
        for round in 0..3 {
            for i in 0..4u8 {
                let msg = [round as u8 * 10 + i; 8];
                hdr.try_push(&msg).unwrap();
            }
            for i in 0..4u8 {
                let len = hdr.try_pop(&mut buf).unwrap();
                assert_eq!(len, 8);
                assert_eq!(buf[0], round as u8 * 10 + i);
            }
        }
    }

    #[test]
    fn test_slot_ptr_uses_correct_offset() {
        // Verify that different slots go to different memory locations
        let capacity: u64 = 4;
        let slot_size: u64 = 64;
        let size = RingHeader::region_size(capacity, slot_size);
        let mut region = vec![0u8; size];

        unsafe { RingHeader::init(region.as_mut_ptr(), capacity, slot_size); }
        let hdr = unsafe { &*(region.as_ptr() as *const RingHeader) };

        // Push different data to each slot
        hdr.try_push(b"AAAA").unwrap();
        hdr.try_push(b"BBBB").unwrap();
        hdr.try_push(b"CCCC").unwrap();

        let mut buf = [0u8; 64];
        let len = hdr.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"AAAA");
        let len = hdr.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"BBBB");
        let len = hdr.try_pop(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"CCCC");
    }
}
