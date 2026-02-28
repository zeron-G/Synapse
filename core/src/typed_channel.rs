//! Schema-driven typed channels over raw ring buffers.
//!
//! `TypedChannel<T>` binds IDL-generated `#[repr(C)]` types to ring buffer slots,
//! enabling zero-copy typed reads and writes. A `ChannelRegistry` maps channel names
//! to slot ranges, supporting multiple independent channels in a single shm segment.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::{Result, SynapseError};
use crate::ring::{Ring, RingHeader};

/// Maximum number of channels in a single registry.
pub const MAX_CHANNELS: usize = 64;

/// Size of a single channel entry in the registry (name + offsets).
const CHANNEL_NAME_LEN: usize = 48;

/// Registry header size: count(4) + padding(4) + entries.
const REGISTRY_HEADER_SIZE: usize = 8;

/// Size of a single registry entry.
const REGISTRY_ENTRY_SIZE: usize = CHANNEL_NAME_LEN + 24; // name(48) + offset(8) + capacity(8) + slot_size(8)

/// Total registry size.
pub const REGISTRY_SIZE: usize = REGISTRY_HEADER_SIZE + MAX_CHANNELS * REGISTRY_ENTRY_SIZE;

/// A registry entry stored in shared memory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ChannelEntry {
    /// Null-terminated channel name (max 47 chars + null).
    pub name: [u8; CHANNEL_NAME_LEN],
    /// Byte offset from the start of the shared region to this channel's ring.
    pub offset: u64,
    /// Ring capacity (number of slots).
    pub capacity: u64,
    /// Slot size in bytes.
    pub slot_size: u64,
}

/// Channel descriptor returned from registry lookups.
#[derive(Debug, Clone)]
pub struct ChannelDescriptor {
    pub name: String,
    pub offset: usize,
    pub capacity: u64,
    pub slot_size: u64,
}

/// Registry mapping channel names to ring buffer locations within a shared memory segment.
///
/// Layout in shared memory:
///   [0..4)    channel_count (AtomicU32)
///   [4..8)    padding
///   [8..)     entries (MAX_CHANNELS * REGISTRY_ENTRY_SIZE)
pub struct ChannelRegistry {
    base: *mut u8,
}

unsafe impl Send for ChannelRegistry {}
unsafe impl Sync for ChannelRegistry {}

impl ChannelRegistry {
    /// Initialize a channel registry at the given pointer.
    ///
    /// # Safety
    /// `base` must point to at least `REGISTRY_SIZE` bytes of writable, zeroed memory.
    pub unsafe fn init(base: *mut u8) {
        // Set channel count to 0
        let count_ptr = base as *mut AtomicU32;
        (*count_ptr).store(0, Ordering::Release);
    }

    /// Create a registry view over existing shared memory.
    ///
    /// # Safety
    /// `base` must point to a previously initialized registry region.
    pub unsafe fn from_ptr(base: *mut u8) -> Self {
        Self { base }
    }

    fn count_atomic(&self) -> &AtomicU32 {
        unsafe { &*(self.base as *const AtomicU32) }
    }

    /// Number of registered channels.
    pub fn count(&self) -> u32 {
        self.count_atomic().load(Ordering::Acquire)
    }

    /// Register a new channel. Returns the entry index.
    ///
    /// # Safety
    /// Must only be called by the host (single writer) during setup.
    pub unsafe fn register(
        &self,
        name: &str,
        offset: u64,
        capacity: u64,
        slot_size: u64,
    ) -> Result<u32> {
        let count = self.count();
        if count as usize >= MAX_CHANNELS {
            return Err(SynapseError::InvalidState(
                "channel registry full".to_string(),
            ));
        }

        if name.len() >= CHANNEL_NAME_LEN {
            return Err(SynapseError::InvalidState(format!(
                "channel name too long: {} (max {})",
                name.len(),
                CHANNEL_NAME_LEN - 1
            )));
        }

        // Check for duplicates
        for i in 0..count {
            let entry = self.entry(i);
            if entry_name(&entry.name) == name {
                return Err(SynapseError::InvalidState(format!(
                    "duplicate channel name: {name}"
                )));
            }
        }

        let entry_ptr = self.entry_ptr(count);
        let entry = &mut *(entry_ptr as *mut ChannelEntry);

        // Write name
        let name_bytes = name.as_bytes();
        entry.name[..name_bytes.len()].copy_from_slice(name_bytes);
        entry.name[name_bytes.len()] = 0; // null terminate

        entry.offset = offset;
        entry.capacity = capacity;
        entry.slot_size = slot_size;

        // Increment count (release to make the entry visible)
        self.count_atomic().store(count + 1, Ordering::Release);

        Ok(count)
    }

    /// Look up a channel by name.
    pub fn lookup(&self, name: &str) -> Option<ChannelDescriptor> {
        let count = self.count();
        for i in 0..count {
            let entry = unsafe { self.entry(i) };
            if entry_name(&entry.name) == name {
                return Some(ChannelDescriptor {
                    name: name.to_string(),
                    offset: entry.offset as usize,
                    capacity: entry.capacity,
                    slot_size: entry.slot_size,
                });
            }
        }
        None
    }

    /// Get all registered channels.
    pub fn channels(&self) -> Vec<ChannelDescriptor> {
        let count = self.count();
        (0..count)
            .map(|i| {
                let entry = unsafe { self.entry(i) };
                ChannelDescriptor {
                    name: entry_name(&entry.name).to_string(),
                    offset: entry.offset as usize,
                    capacity: entry.capacity,
                    slot_size: entry.slot_size,
                }
            })
            .collect()
    }

    fn entry_ptr(&self, index: u32) -> *const u8 {
        unsafe {
            self.base
                .add(REGISTRY_HEADER_SIZE + index as usize * REGISTRY_ENTRY_SIZE)
        }
    }

    unsafe fn entry(&self, index: u32) -> ChannelEntry {
        let ptr = self.entry_ptr(index) as *const ChannelEntry;
        std::ptr::read(ptr)
    }
}

/// Extract a channel name from the fixed-size name buffer.
fn entry_name(name_buf: &[u8; CHANNEL_NAME_LEN]) -> &str {
    let end = name_buf
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(CHANNEL_NAME_LEN);
    std::str::from_utf8(&name_buf[..end]).unwrap_or("")
}

/// A typed channel that reads and writes `#[repr(C)]` values over a ring buffer.
///
/// `T` must be a `#[repr(C)]` type whose size fits within the ring's slot payload.
/// Zero-copy: values are written directly into ring buffer slots.
pub struct TypedChannel<T: Copy> {
    ring: Ring,
    _marker: PhantomData<T>,
}

impl<T: Copy> TypedChannel<T> {
    /// Create a typed channel over a ring buffer.
    ///
    /// # Safety
    /// `ring_base` must point to a properly initialized ring header region.
    /// `T` must be `#[repr(C)]` and `size_of::<T>()` must fit in the ring's slot payload.
    pub unsafe fn from_ring_ptr(ring_base: *mut u8) -> Result<Self> {
        let ring = Ring::from_ptr(ring_base);

        let type_size = std::mem::size_of::<T>();
        let max_payload = ring.slot_payload_size();

        if type_size > max_payload {
            return Err(SynapseError::DataTooLarge {
                data_len: type_size,
                slot_size: max_payload,
            });
        }

        Ok(Self {
            ring,
            _marker: PhantomData,
        })
    }

    /// Write a typed value into the channel.
    pub fn write(&self, value: &T) -> Result<()> {
        let size = std::mem::size_of::<T>();
        let bytes = unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size) };
        self.ring.try_push(bytes)
    }

    /// Read a typed value from the channel (non-blocking).
    pub fn read(&self) -> Option<T> {
        let size = std::mem::size_of::<T>();
        let mut buf = vec![0u8; size];
        match self.ring.try_pop(&mut buf) {
            Ok(len) if len == size => {
                let value = unsafe { std::ptr::read(buf.as_ptr() as *const T) };
                Some(value)
            }
            _ => None,
        }
    }

    /// Number of items in the channel.
    pub fn len(&self) -> u64 {
        self.ring.len()
    }

    /// Whether the channel is empty.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// Compute the total shared memory size needed for a set of channels.
///
/// Layout: [ControlBlock(256)] [Registry(REGISTRY_SIZE)] [Ring0] [Ring1] ...
pub fn compute_multi_channel_size(
    channels: &[(u64, u64)], // (capacity, slot_size) per channel
) -> usize {
    let mut total = 256 + REGISTRY_SIZE; // control block + registry
    for &(cap, slot_size) in channels {
        total += RingHeader::region_size(cap, slot_size);
    }
    total
}

/// Compute ring offsets for multiple channels within a shared memory region.
///
/// Returns a vec of byte offsets from the start of the region.
pub fn compute_channel_offsets(channels: &[(u64, u64)]) -> Vec<usize> {
    let mut offset = 256 + REGISTRY_SIZE; // after control block + registry
    let mut offsets = Vec::with_capacity(channels.len());
    for &(cap, slot_size) in channels {
        offsets.push(offset);
        offset += RingHeader::region_size(cap, slot_size);
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Vec3f {
        x: f32,
        y: f32,
        z: f32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct GameState {
        x: f32,
        y: f32,
        z: f32,
        health: f32,
        frame_id: u64,
    }

    fn alloc_typed_ring<T: Copy>(capacity: u64) -> (Vec<u8>, TypedChannel<T>) {
        let type_size = std::mem::size_of::<T>();
        // slot_size = 4-byte length prefix + type_size, rounded up to 8-byte alignment
        let slot_size = ((4 + type_size + 7) & !7) as u64;
        let size = RingHeader::region_size(capacity, slot_size);
        let mut region = vec![0u8; size];
        unsafe {
            RingHeader::init(region.as_mut_ptr(), capacity, slot_size);
            let ch = TypedChannel::<T>::from_ring_ptr(region.as_mut_ptr()).unwrap();
            (region, ch)
        }
    }

    #[test]
    fn test_typed_channel_write_read() {
        let (_mem, ch) = alloc_typed_ring::<Vec3f>(16);

        let v = Vec3f {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        };
        ch.write(&v).unwrap();
        assert_eq!(ch.len(), 1);

        let got = ch.read().unwrap();
        assert_eq!(got, v);
        assert!(ch.is_empty());
    }

    #[test]
    fn test_typed_channel_multiple_values() {
        let (_mem, ch) = alloc_typed_ring::<GameState>(8);

        for i in 0..8u32 {
            let state = GameState {
                x: i as f32,
                y: i as f32 * 2.0,
                z: i as f32 * 3.0,
                health: 100.0 - i as f32,
                frame_id: i as u64,
            };
            ch.write(&state).unwrap();
        }

        assert_eq!(ch.len(), 8);

        for i in 0..8u32 {
            let state = ch.read().unwrap();
            assert_eq!(state.frame_id, i as u64);
            assert_eq!(state.health, 100.0 - i as f32);
        }
    }

    #[test]
    fn test_typed_channel_type_too_large() {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Huge {
            data: [u8; 512],
        }

        // slot_size of 32 means max payload = 28, way smaller than 512
        let size = RingHeader::region_size(4, 32);
        let mut region = vec![0u8; size];
        unsafe {
            RingHeader::init(region.as_mut_ptr(), 4, 32);
            let result = TypedChannel::<Huge>::from_ring_ptr(region.as_mut_ptr());
            assert!(result.is_err());
            match result.err().unwrap() {
                SynapseError::DataTooLarge { .. } => {}
                e => panic!("expected DataTooLarge, got {e:?}"),
            }
        }
    }

    #[test]
    fn test_registry_init_and_register() {
        let mut mem = vec![0u8; REGISTRY_SIZE];
        unsafe {
            ChannelRegistry::init(mem.as_mut_ptr());
            let reg = ChannelRegistry::from_ptr(mem.as_mut_ptr());

            assert_eq!(reg.count(), 0);

            reg.register("positions", 1024, 64, 128).unwrap();
            assert_eq!(reg.count(), 1);

            reg.register("commands", 2048, 32, 64).unwrap();
            assert_eq!(reg.count(), 2);
        }
    }

    #[test]
    fn test_registry_lookup() {
        let mut mem = vec![0u8; REGISTRY_SIZE];
        unsafe {
            ChannelRegistry::init(mem.as_mut_ptr());
            let reg = ChannelRegistry::from_ptr(mem.as_mut_ptr());

            reg.register("positions", 1024, 64, 128).unwrap();
            reg.register("commands", 2048, 32, 64).unwrap();

            let desc = reg.lookup("positions").unwrap();
            assert_eq!(desc.name, "positions");
            assert_eq!(desc.offset, 1024);
            assert_eq!(desc.capacity, 64);
            assert_eq!(desc.slot_size, 128);

            let desc = reg.lookup("commands").unwrap();
            assert_eq!(desc.name, "commands");
            assert_eq!(desc.offset, 2048);

            assert!(reg.lookup("nonexistent").is_none());
        }
    }

    #[test]
    fn test_registry_duplicate_name() {
        let mut mem = vec![0u8; REGISTRY_SIZE];
        unsafe {
            ChannelRegistry::init(mem.as_mut_ptr());
            let reg = ChannelRegistry::from_ptr(mem.as_mut_ptr());

            reg.register("test", 1024, 64, 128).unwrap();
            let result = reg.register("test", 2048, 32, 64);
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_registry_channels_list() {
        let mut mem = vec![0u8; REGISTRY_SIZE];
        unsafe {
            ChannelRegistry::init(mem.as_mut_ptr());
            let reg = ChannelRegistry::from_ptr(mem.as_mut_ptr());

            reg.register("alpha", 100, 16, 32).unwrap();
            reg.register("beta", 200, 32, 64).unwrap();
            reg.register("gamma", 300, 64, 128).unwrap();

            let channels = reg.channels();
            assert_eq!(channels.len(), 3);
            assert_eq!(channels[0].name, "alpha");
            assert_eq!(channels[1].name, "beta");
            assert_eq!(channels[2].name, "gamma");
        }
    }

    #[test]
    fn test_compute_multi_channel_size() {
        let channels = vec![(16, 64u64), (32, 128)];
        let size = compute_multi_channel_size(&channels);

        let expected = 256
            + REGISTRY_SIZE
            + RingHeader::region_size(16, 64)
            + RingHeader::region_size(32, 128);
        assert_eq!(size, expected);
    }

    #[test]
    fn test_compute_channel_offsets() {
        let channels = vec![(16, 64u64), (32, 128)];
        let offsets = compute_channel_offsets(&channels);

        assert_eq!(offsets.len(), 2);
        assert_eq!(offsets[0], 256 + REGISTRY_SIZE);
        assert_eq!(
            offsets[1],
            256 + REGISTRY_SIZE + RingHeader::region_size(16, 64)
        );
    }

    #[test]
    fn test_typed_channel_with_registry() {
        // End-to-end: create a region with registry + ring, register channel, look it up, use it
        let capacity = 16u64;
        let type_size = std::mem::size_of::<Vec3f>();
        let slot_size = ((4 + type_size + 7) & !7) as u64;

        let ring_size = RingHeader::region_size(capacity, slot_size);
        let total_size = REGISTRY_SIZE + ring_size;

        let mut mem = vec![0u8; total_size];
        let base = mem.as_mut_ptr();

        unsafe {
            // Init registry at start
            ChannelRegistry::init(base);
            let reg = ChannelRegistry::from_ptr(base);

            // Init ring after registry
            let ring_offset = REGISTRY_SIZE;
            RingHeader::init(base.add(ring_offset), capacity, slot_size);

            // Register the channel
            reg.register("positions", ring_offset as u64, capacity, slot_size)
                .unwrap();

            // Look it up
            let desc = reg.lookup("positions").unwrap();
            assert_eq!(desc.offset, ring_offset);

            // Create typed channel from the descriptor
            let ch = TypedChannel::<Vec3f>::from_ring_ptr(base.add(desc.offset)).unwrap();

            // Write and read
            let v = Vec3f {
                x: 42.0,
                y: -1.0,
                z: 0.5,
            };
            ch.write(&v).unwrap();
            let got = ch.read().unwrap();
            assert_eq!(got, v);
        }
    }
}
