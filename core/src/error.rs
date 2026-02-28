//! Error types for Synapse.

use std::fmt;

/// All errors that can occur in Synapse operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynapseError {
    /// Shared memory creation/opening failed.
    ShmError(String),
    /// Ring buffer is full (non-blocking push failed).
    RingFull,
    /// Ring buffer is empty (non-blocking pop failed).
    RingEmpty,
    /// Data exceeds slot size.
    DataTooLarge { data_len: usize, slot_size: usize },
    /// Magic number mismatch — not a Synapse region.
    BadMagic { expected: u64, found: u64 },
    /// Version mismatch.
    VersionMismatch { expected: u32, found: u32 },
    /// Session token mismatch — wrong bridge instance.
    SessionMismatch,
    /// Invalid state transition.
    InvalidState(String),
    /// OS-level I/O error.
    Io(String),
}

impl fmt::Display for SynapseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShmError(msg) => write!(f, "shared memory error: {msg}"),
            Self::RingFull => write!(f, "ring buffer full"),
            Self::RingEmpty => write!(f, "ring buffer empty"),
            Self::DataTooLarge { data_len, slot_size } => {
                write!(f, "data ({data_len} bytes) exceeds slot size ({slot_size} bytes)")
            }
            Self::BadMagic { expected, found } => {
                write!(f, "bad magic: expected {expected:#018x}, found {found:#018x}")
            }
            Self::VersionMismatch { expected, found } => {
                write!(f, "version mismatch: expected {expected}, found {found}")
            }
            Self::SessionMismatch => write!(f, "session token mismatch"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for SynapseError {}

impl From<std::io::Error> for SynapseError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, SynapseError>;
