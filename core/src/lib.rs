//! Synapse — Cross-language runtime bridge using shared memory + lock-free ring buffers.
//!
//! # Quick Start
//!
//! ```no_run
//! use synapse_core::{host, connect};
//!
//! // Process A (host)
//! let bridge = host("my_channel").unwrap();
//! bridge.send(b"hello from A").unwrap();
//!
//! // Process B (connect)
//! let bridge = connect("my_channel").unwrap();
//! let data = bridge.recv().unwrap();
//! ```

pub mod control;
pub mod error;
pub mod ring;
pub mod shm;
pub mod typed_channel;

use control::{ControlBlock, State};
use error::{Result, SynapseError};
use ring::{Ring, RingHeader};
use shm::SharedRegion;

/// Default slot size (256 bytes, including 4-byte length prefix → 252 bytes max payload).
pub const DEFAULT_SLOT_SIZE: u64 = 256;

/// Default ring capacity (1024 slots).
pub const DEFAULT_CAPACITY: u64 = 1024;

/// Size of the control block region (256 bytes).
const CONTROL_SIZE: usize = 256;

/// A Synapse bridge endpoint.
pub struct Bridge {
    _region: SharedRegion,
    is_host: bool,
    session_token: u128,
    ring_ab_offset: usize,
    ring_ba_offset: usize,
    slot_size: u64,
}

impl Bridge {
    fn ring_ab(&self) -> Ring {
        unsafe { Ring::from_ptr(self._region.as_ptr().add(self.ring_ab_offset)) }
    }

    fn ring_ba(&self) -> Ring {
        unsafe { Ring::from_ptr(self._region.as_ptr().add(self.ring_ba_offset)) }
    }

    /// Send data through the bridge.
    /// Host sends via ring_ab (A→B), Connector sends via ring_ba (B→A).
    pub fn send(&self, data: &[u8]) -> Result<()> {
        if self.is_host {
            self.ring_ab().try_push(data)
        } else {
            self.ring_ba().try_push(data)
        }
    }

    /// Receive data from the bridge (non-blocking).
    /// Host reads from ring_ba (B→A), Connector reads from ring_ab (A→B).
    pub fn recv(&self) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; self.slot_size as usize];
        let ring = if self.is_host {
            self.ring_ba()
        } else {
            self.ring_ab()
        };
        match ring.try_pop(&mut buf) {
            Ok(len) => {
                buf.truncate(len);
                Some(buf)
            }
            Err(SynapseError::RingEmpty) => None,
            Err(_) => None,
        }
    }

    /// Check if the bridge is in Ready state.
    pub fn is_ready(&self) -> bool {
        let cb = unsafe { &*(self._region.as_ptr() as *const ControlBlock) };
        cb.state() == State::Ready
    }

    /// Get the session token.
    pub fn session_token(&self) -> u128 {
        self.session_token
    }
}

fn region_size(capacity: u64, slot_size: u64) -> usize {
    let ring_size = RingHeader::region_size(capacity, slot_size);
    CONTROL_SIZE + ring_size * 2
}

/// Create a new Synapse bridge as the host.
pub fn host(name: &str) -> Result<Bridge> {
    host_with_config(name, DEFAULT_CAPACITY, DEFAULT_SLOT_SIZE)
}

/// Create a new Synapse bridge as the host with custom config.
pub fn host_with_config(name: &str, capacity: u64, slot_size: u64) -> Result<Bridge> {
    assert!(capacity.is_power_of_two(), "capacity must be power of 2");
    assert!(slot_size >= 8, "slot_size must be >= 8");

    let total_size = region_size(capacity, slot_size);
    let region = SharedRegion::create(name, total_size)?;

    // Generate random session token
    let session_token: u128 = {
        let mut buf = [0u8; 16];
        #[cfg(unix)]
        {
            use std::io::Read;
            let _ = std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut buf).map(|_| ()));
        }
        #[cfg(windows)]
        {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            buf.copy_from_slice(&t.to_le_bytes());
        }
        u128::from_le_bytes(buf)
    };

    let ring_ab_offset = CONTROL_SIZE;
    let ring_ba_offset = CONTROL_SIZE + RingHeader::region_size(capacity, slot_size);

    unsafe {
        ControlBlock::init(region.as_ptr(), total_size, session_token);
        RingHeader::init(region.as_ptr().add(ring_ab_offset), capacity, slot_size);
        RingHeader::init(region.as_ptr().add(ring_ba_offset), capacity, slot_size);

        let cb = &*(region.as_ptr() as *const ControlBlock);
        cb.set_state(State::Ready);
    }

    Ok(Bridge {
        _region: region,
        is_host: true,
        session_token,
        ring_ab_offset,
        ring_ba_offset,
        slot_size,
    })
}

/// Connect to an existing Synapse bridge.
pub fn connect(name: &str) -> Result<Bridge> {
    connect_with_config(name, DEFAULT_CAPACITY, DEFAULT_SLOT_SIZE)
}

/// Connect with custom config.
pub fn connect_with_config(name: &str, capacity: u64, slot_size: u64) -> Result<Bridge> {
    let total_size = region_size(capacity, slot_size);
    let region = SharedRegion::open(name, total_size)?;

    let cb = unsafe { ControlBlock::validate(region.as_ptr())? };
    let session_token = cb.session_token();

    unsafe {
        let cb_mut = &mut *(region.as_ptr() as *mut ControlBlock);
        cb_mut.connector_pid = std::process::id() as u64;
    }

    let ring_ab_offset = CONTROL_SIZE;
    let ring_ba_offset = CONTROL_SIZE + RingHeader::region_size(capacity, slot_size);

    Ok(Bridge {
        _region: region,
        is_host: false,
        session_token,
        ring_ab_offset,
        ring_ba_offset,
        slot_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_connect_roundtrip() {
        let name = "synapse_test_rt";
        let _ = std::fs::remove_file(format!("/dev/shm/{name}"));

        let h = host(name).expect("host failed");
        let c = connect(name).expect("connect failed");

        h.send(b"hello from host").unwrap();
        let msg = c.recv().expect("no message");
        assert_eq!(msg, b"hello from host");

        c.send(b"hello from connector").unwrap();
        let msg = h.recv().expect("no message");
        assert_eq!(msg, b"hello from connector");
    }

    #[test]
    fn test_session_token_matches() {
        let name = "synapse_test_sess";
        let _ = std::fs::remove_file(format!("/dev/shm/{name}"));

        let h = host(name).expect("host failed");
        let c = connect(name).expect("connect failed");

        assert_eq!(h.session_token(), c.session_token());
        assert_ne!(h.session_token(), 0);
    }

    #[test]
    fn test_bidirectional_multiple() {
        let name = "synapse_test_bd";
        let _ = std::fs::remove_file(format!("/dev/shm/{name}"));

        let h = host(name).unwrap();
        let c = connect(name).unwrap();

        for i in 0..100u32 {
            h.send(format!("msg_{i}").as_bytes()).unwrap();
        }
        for i in 0..100u32 {
            let data = c.recv().unwrap();
            assert_eq!(data, format!("msg_{i}").as_bytes());
        }

        for i in 0..50u32 {
            c.send(format!("reply_{i}").as_bytes()).unwrap();
        }
        for i in 0..50u32 {
            let data = h.recv().unwrap();
            assert_eq!(data, format!("reply_{i}").as_bytes());
        }
    }
}
