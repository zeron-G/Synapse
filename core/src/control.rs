//! Control block for Synapse shared memory regions.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Magic number: "SYNAPSE\0" as little-endian u64.
pub const MAGIC: u64 = 0x53594E4150534500;

/// Current protocol version.
pub const VERSION: u32 = 1;

/// Connection state machine.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Region created, waiting for connector.
    Init = 0,
    /// Both sides connected, ready for data.
    Ready = 1,
    /// Graceful shutdown in progress.
    Closing = 2,
    /// Dead — peer lost or fully shut down.
    Dead = 3,
}

impl State {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Init),
            1 => Some(Self::Ready),
            2 => Some(Self::Closing),
            3 => Some(Self::Dead),
            _ => None,
        }
    }
}

/// Control block layout at offset 0 of the shared memory region.
///
/// Total size: 256 bytes (padded for alignment).
/// Contains magic, version, session token, heartbeats, and state.
#[repr(C, align(64))]
pub struct ControlBlock {
    /// Magic number for identification (0x53594E4150534500).
    pub magic: u64,
    /// Protocol version.
    pub version: u32,
    /// Flags (reserved).
    pub flags: u32,
    /// Total region size in bytes.
    pub region_size: u64,
    /// Creator process ID.
    pub creator_pid: u64,
    /// Connector process ID (0 until connected).
    pub connector_pid: u64,
    /// Random session token (u128) to prevent cross-attach.
    /// Stored as two u64s for alignment simplicity.
    pub session_token_lo: u64,
    pub session_token_hi: u64,
    /// Creator heartbeat counter (monotonically increasing).
    pub creator_heartbeat: AtomicU64,
    /// Connector heartbeat counter.
    pub connector_heartbeat: AtomicU64,
    /// Connection state (see `State` enum).
    pub state: AtomicU32,
    /// Number of channels.
    pub channel_count: u32,
    /// Padding to 256 bytes.
    pub _reserved: [u8; 128],
}

const _: () = {
    // Ensure ControlBlock fits within 256 bytes.
    assert!(std::mem::size_of::<ControlBlock>() <= 256);
};

impl ControlBlock {
    /// Initialize a new control block at the given pointer.
    ///
    /// # Safety
    /// `ptr` must point to at least 256 bytes of writable, zeroed memory.
    pub unsafe fn init(ptr: *mut u8, region_size: usize, session_token: u128) {
        let cb = &mut *(ptr as *mut ControlBlock);
        cb.magic = MAGIC;
        cb.version = VERSION;
        cb.flags = 0;
        cb.region_size = region_size as u64;
        cb.creator_pid = std::process::id() as u64;
        cb.connector_pid = 0;
        cb.session_token_lo = session_token as u64;
        cb.session_token_hi = (session_token >> 64) as u64;
        cb.creator_heartbeat = AtomicU64::new(0);
        cb.connector_heartbeat = AtomicU64::new(0);
        cb.state = AtomicU32::new(State::Init as u32);
        cb.channel_count = 1;
    }

    /// Validate magic and version. Returns the control block reference.
    ///
    /// # Safety
    /// `ptr` must point to a valid ControlBlock in shared memory.
    pub unsafe fn validate(ptr: *const u8) -> crate::error::Result<&'static ControlBlock> {
        let cb = &*(ptr as *const ControlBlock);
        if cb.magic != MAGIC {
            return Err(crate::error::SynapseError::BadMagic {
                expected: MAGIC,
                found: cb.magic,
            });
        }
        if cb.version != VERSION {
            return Err(crate::error::SynapseError::VersionMismatch {
                expected: VERSION,
                found: cb.version,
            });
        }
        Ok(cb)
    }

    /// Get session token as u128.
    pub fn session_token(&self) -> u128 {
        (self.session_token_hi as u128) << 64 | self.session_token_lo as u128
    }

    /// Get current state.
    pub fn state(&self) -> State {
        State::from_u32(self.state.load(Ordering::Acquire)).unwrap_or(State::Dead)
    }

    /// Transition to a new state.
    pub fn set_state(&self, new_state: State) {
        self.state.store(new_state as u32, Ordering::Release);
    }
}
