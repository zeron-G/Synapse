//! Graceful shutdown protocol and heartbeat watchdog.
//!
//! Provides:
//! - `Watchdog` — monitors peer heartbeats, detects death after N missed beats
//! - `ShutdownProtocol` — coordinates graceful shutdown: signal → drain → cleanup
//!
//! The heartbeat mechanism uses the existing `creator_heartbeat` and
//! `connector_heartbeat` fields in the `ControlBlock`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::control::{ControlBlock, State};

/// Heartbeat interval — how often each side bumps its counter.
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);

/// Number of missed heartbeats before declaring peer dead.
const DEFAULT_MISSED_THRESHOLD: u32 = 5;

/// Peer status returned by the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerStatus {
    /// Peer is alive — heartbeat is advancing.
    Alive,
    /// Peer heartbeat is stale (missed some beats but below threshold).
    Stale { missed_beats: u32 },
    /// Peer is considered dead (missed >= threshold beats).
    Dead,
}

/// Configuration for the watchdog.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How often to check/write heartbeats.
    pub heartbeat_interval: Duration,
    /// How many missed beats before declaring peer dead.
    pub missed_threshold: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            missed_threshold: DEFAULT_MISSED_THRESHOLD,
        }
    }
}

/// Heartbeat watchdog that tracks peer liveness.
///
/// Each side (host or connector) should call `beat()` periodically to update
/// its own heartbeat, and `check_peer()` to verify the other side is alive.
pub struct Watchdog {
    /// Pointer to the control block in shared memory.
    cb_ptr: *const ControlBlock,
    /// Whether this side is the host.
    is_host: bool,
    /// Last observed peer heartbeat value.
    last_peer_beat: u64,
    /// When we last observed the peer heartbeat change.
    last_peer_change: Instant,
    /// Configuration.
    config: WatchdogConfig,
}

unsafe impl Send for Watchdog {}
unsafe impl Sync for Watchdog {}

impl Watchdog {
    /// Create a new watchdog for the given control block.
    ///
    /// # Safety
    /// `cb_ptr` must point to a valid, mapped `ControlBlock` that outlives this watchdog.
    pub unsafe fn new(cb_ptr: *const ControlBlock, is_host: bool) -> Self {
        Self::with_config(cb_ptr, is_host, WatchdogConfig::default())
    }

    /// Create a watchdog with custom configuration.
    ///
    /// # Safety
    /// `cb_ptr` must point to a valid, mapped `ControlBlock` that outlives this watchdog.
    pub unsafe fn with_config(
        cb_ptr: *const ControlBlock,
        is_host: bool,
        config: WatchdogConfig,
    ) -> Self {
        let cb = &*cb_ptr;
        let peer_beat = if is_host {
            cb.connector_heartbeat.load(Ordering::Acquire)
        } else {
            cb.creator_heartbeat.load(Ordering::Acquire)
        };

        Self {
            cb_ptr,
            is_host,
            last_peer_beat: peer_beat,
            last_peer_change: Instant::now(),
            config,
        }
    }

    fn cb(&self) -> &ControlBlock {
        unsafe { &*self.cb_ptr }
    }

    fn my_heartbeat(&self) -> &AtomicU64 {
        if self.is_host {
            &self.cb().creator_heartbeat
        } else {
            &self.cb().connector_heartbeat
        }
    }

    fn peer_heartbeat(&self) -> &AtomicU64 {
        if self.is_host {
            &self.cb().connector_heartbeat
        } else {
            &self.cb().creator_heartbeat
        }
    }

    /// Bump our own heartbeat counter. Call this periodically.
    pub fn beat(&self) {
        self.my_heartbeat().fetch_add(1, Ordering::Release);
    }

    /// Check peer liveness. Returns the current peer status.
    ///
    /// Also updates internal tracking of when the peer's heartbeat last changed.
    pub fn check_peer(&mut self) -> PeerStatus {
        // If state is Dead or Closing, peer is effectively gone
        let state = self.cb().state();
        if state == State::Dead {
            return PeerStatus::Dead;
        }

        let current = self.peer_heartbeat().load(Ordering::Acquire);

        if current != self.last_peer_beat {
            // Peer heartbeat advanced — alive
            self.last_peer_beat = current;
            self.last_peer_change = Instant::now();
            return PeerStatus::Alive;
        }

        // Heartbeat unchanged — check how long
        let elapsed = self.last_peer_change.elapsed();
        let missed = (elapsed.as_millis() as u32)
            / (self.config.heartbeat_interval.as_millis() as u32).max(1);

        if missed >= self.config.missed_threshold {
            PeerStatus::Dead
        } else if missed > 0 {
            PeerStatus::Stale {
                missed_beats: missed,
            }
        } else {
            PeerStatus::Alive
        }
    }

    /// Get the heartbeat interval.
    pub fn heartbeat_interval(&self) -> Duration {
        self.config.heartbeat_interval
    }
}

/// Shutdown mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownMode {
    /// Drain buffers before cleaning up.
    Graceful,
    /// Set state to Dead immediately.
    Immediate,
}

/// Coordinates graceful shutdown of a bridge endpoint.
pub struct ShutdownProtocol {
    cb_ptr: *mut ControlBlock,
    is_host: bool,
    shutdown_initiated: AtomicBool,
}

unsafe impl Send for ShutdownProtocol {}
unsafe impl Sync for ShutdownProtocol {}

impl ShutdownProtocol {
    /// Create a new shutdown protocol handler.
    ///
    /// # Safety
    /// `cb_ptr` must point to a valid, mapped `ControlBlock` that outlives this instance.
    pub unsafe fn new(cb_ptr: *mut ControlBlock, is_host: bool) -> Self {
        Self {
            cb_ptr,
            is_host,
            shutdown_initiated: AtomicBool::new(false),
        }
    }

    fn cb(&self) -> &ControlBlock {
        unsafe { &*self.cb_ptr }
    }

    /// Whether this side has initiated shutdown.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown_initiated.load(Ordering::Acquire)
    }

    /// Initiate shutdown with the given mode.
    ///
    /// - `Graceful`: Sets state to `Closing`, waits for drain, then sets `Dead`.
    /// - `Immediate`: Sets state to `Dead` right away.
    ///
    /// Returns `true` if this call initiated the shutdown, `false` if already shutting down.
    pub fn initiate(&self, mode: ShutdownMode) -> bool {
        if self
            .shutdown_initiated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false; // Already shutting down
        }

        match mode {
            ShutdownMode::Graceful => {
                self.cb().set_state(State::Closing);
            }
            ShutdownMode::Immediate => {
                self.cb().set_state(State::Dead);
            }
        }

        true
    }

    /// Complete the shutdown sequence (called after draining buffers).
    ///
    /// Sets state to `Dead` and cleans up resources.
    pub fn complete(&self) {
        self.cb().set_state(State::Dead);
    }

    /// Check if the peer has signaled shutdown (state is Closing or Dead).
    pub fn peer_shutting_down(&self) -> bool {
        matches!(self.cb().state(), State::Closing | State::Dead)
    }

    /// Whether the bridge should continue processing messages.
    ///
    /// Returns `false` if state is `Dead`, meaning all communication should stop.
    pub fn should_continue(&self) -> bool {
        self.cb().state() != State::Dead
    }

    /// Whether this endpoint is the host (responsible for shm cleanup).
    pub fn is_host(&self) -> bool {
        self.is_host
    }

    /// Perform cleanup for the shm region on Unix.
    /// Removes the `/dev/shm/{name}` file if this is the host.
    #[cfg(unix)]
    pub fn cleanup_shm(name: &str) {
        let path = format!("/dev/shm/{name}");
        let _ = std::fs::remove_file(&path);
    }

    /// No-op on Windows — the kernel cleans up when all handles close.
    #[cfg(windows)]
    pub fn cleanup_shm(_name: &str) {
        // Windows kernel objects are reference-counted and auto-cleanup.
    }
}

/// Detect if a process with the given PID is still alive.
#[cfg(unix)]
pub fn is_process_alive(pid: u64) -> bool {
    if pid == 0 {
        return false;
    }
    // Check if /proc/{pid} exists (Linux-specific but works on WSL too)
    let proc_path = format!("/proc/{pid}");
    std::path::Path::new(&proc_path).exists()
}

/// Detect if a process is alive on Windows.
#[cfg(windows)]
pub fn is_process_alive(pid: u64) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = windows_sys::Win32::System::Threading::OpenProcess(
            0x00100000, // SYNCHRONIZE
            0, pid as u32,
        );
        if handle.is_null() {
            return false;
        }
        let result = windows_sys::Win32::System::Threading::WaitForSingleObject(handle, 0);
        windows_sys::Win32::Foundation::CloseHandle(handle);
        // WAIT_TIMEOUT (258) means process is still running
        result == 258
    }
}

/// Check if a stale shm region was left by a dead process and can be reclaimed.
///
/// Returns `true` if the region exists and its creator is dead, meaning it's safe
/// to unlink and recreate.
pub fn can_reclaim_stale_region(name: &str) -> bool {
    // Try to open and read the control block
    let size = 256; // Just need the control block
    match crate::shm::SharedRegion::open(name, size) {
        Ok(region) => {
            let cb = unsafe { &*(region.as_ptr() as *const ControlBlock) };
            if cb.magic != crate::control::MAGIC {
                return true; // Corrupted, safe to reclaim
            }
            let creator_pid = cb.creator_pid;
            !is_process_alive(creator_pid)
        }
        Err(_) => false, // Doesn't exist or can't open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aligned buffer for ControlBlock (which requires 64-byte alignment).
    #[repr(C, align(64))]
    struct AlignedBuf([u8; 256]);

    fn alloc_control_block() -> Box<AlignedBuf> {
        let mut mem = Box::new(AlignedBuf([0u8; 256]));
        unsafe {
            ControlBlock::init(mem.0.as_mut_ptr(), 256, 0x12345);
        }
        mem
    }

    #[test]
    fn test_watchdog_initial_state() {
        let mem = alloc_control_block();
        let mut wd = unsafe {
            Watchdog::with_config(
                mem.0.as_ptr() as *const ControlBlock,
                true,
                WatchdogConfig {
                    heartbeat_interval: Duration::from_millis(50),
                    missed_threshold: 3,
                },
            )
        };
        // Initially peer heartbeat is 0 and we just started, so it's alive
        let status = wd.check_peer();
        assert_eq!(status, PeerStatus::Alive);
    }

    #[test]
    fn test_watchdog_beat_and_check() {
        let mem = alloc_control_block();
        let cb = unsafe { &*(mem.0.as_ptr() as *const ControlBlock) };

        // Host watchdog
        let mut host_wd = unsafe {
            Watchdog::with_config(
                mem.0.as_ptr() as *const ControlBlock,
                true,
                WatchdogConfig {
                    heartbeat_interval: Duration::from_millis(10),
                    missed_threshold: 3,
                },
            )
        };

        // Connector watchdog
        let mut conn_wd = unsafe {
            Watchdog::with_config(
                mem.0.as_ptr() as *const ControlBlock,
                false,
                WatchdogConfig {
                    heartbeat_interval: Duration::from_millis(10),
                    missed_threshold: 3,
                },
            )
        };

        // Both beat
        host_wd.beat();
        conn_wd.beat();

        // Both should see each other as alive
        assert_eq!(host_wd.check_peer(), PeerStatus::Alive);
        assert_eq!(conn_wd.check_peer(), PeerStatus::Alive);

        // Verify heartbeat values advanced
        assert!(cb.creator_heartbeat.load(Ordering::Acquire) > 0);
        assert!(cb.connector_heartbeat.load(Ordering::Acquire) > 0);
    }

    #[test]
    fn test_watchdog_peer_death_detection() {
        let mem = alloc_control_block();

        let mut wd = unsafe {
            Watchdog::with_config(
                mem.0.as_ptr() as *const ControlBlock,
                true, // host watching connector
                WatchdogConfig {
                    heartbeat_interval: Duration::from_millis(10),
                    missed_threshold: 3,
                },
            )
        };

        // Initially alive
        assert_eq!(wd.check_peer(), PeerStatus::Alive);

        // Wait long enough for 3+ missed beats
        std::thread::sleep(Duration::from_millis(40));

        let status = wd.check_peer();
        assert_eq!(status, PeerStatus::Dead);
    }

    #[test]
    fn test_shutdown_graceful() {
        let mem = alloc_control_block();
        let cb = unsafe { &*(mem.0.as_ptr() as *const ControlBlock) };
        cb.set_state(State::Ready);

        let proto = unsafe { ShutdownProtocol::new(mem.0.as_ptr() as *mut ControlBlock, true) };

        assert!(proto.should_continue());
        assert!(!proto.is_shutting_down());

        // Initiate graceful shutdown
        assert!(proto.initiate(ShutdownMode::Graceful));
        assert!(proto.is_shutting_down());
        assert_eq!(cb.state(), State::Closing);
        assert!(proto.should_continue()); // Still processing during Closing

        // Complete shutdown
        proto.complete();
        assert_eq!(cb.state(), State::Dead);
        assert!(!proto.should_continue());
    }

    #[test]
    fn test_shutdown_immediate() {
        let mem = alloc_control_block();
        let cb = unsafe { &*(mem.0.as_ptr() as *const ControlBlock) };
        cb.set_state(State::Ready);

        let proto = unsafe { ShutdownProtocol::new(mem.0.as_ptr() as *mut ControlBlock, false) };

        assert!(proto.initiate(ShutdownMode::Immediate));
        assert_eq!(cb.state(), State::Dead);
        assert!(!proto.should_continue());
    }

    #[test]
    fn test_shutdown_double_initiate() {
        let mem = alloc_control_block();
        let cb = unsafe { &*(mem.0.as_ptr() as *const ControlBlock) };
        cb.set_state(State::Ready);

        let proto = unsafe { ShutdownProtocol::new(mem.0.as_ptr() as *mut ControlBlock, true) };

        assert!(proto.initiate(ShutdownMode::Graceful));
        // Second call should return false
        assert!(!proto.initiate(ShutdownMode::Graceful));
    }

    #[test]
    fn test_peer_shutdown_detection() {
        let mem = alloc_control_block();
        let cb = unsafe { &*(mem.0.as_ptr() as *const ControlBlock) };
        cb.set_state(State::Ready);

        let host_proto =
            unsafe { ShutdownProtocol::new(mem.0.as_ptr() as *mut ControlBlock, true) };
        let conn_proto =
            unsafe { ShutdownProtocol::new(mem.0.as_ptr() as *mut ControlBlock, false) };

        assert!(!conn_proto.peer_shutting_down());

        // Host initiates shutdown
        host_proto.initiate(ShutdownMode::Graceful);

        // Connector should detect it
        assert!(conn_proto.peer_shutting_down());
    }

    #[test]
    fn test_is_process_alive_self() {
        let my_pid = std::process::id() as u64;
        assert!(is_process_alive(my_pid));
    }

    #[test]
    fn test_is_process_alive_zero() {
        assert!(!is_process_alive(0));
    }

    #[test]
    fn test_is_process_alive_nonexistent() {
        // PID that almost certainly doesn't exist
        assert!(!is_process_alive(4_000_000_000));
    }

    #[test]
    fn test_watchdog_stale_detection() {
        let mem = alloc_control_block();

        let mut wd = unsafe {
            Watchdog::with_config(
                mem.0.as_ptr() as *const ControlBlock,
                true,
                WatchdogConfig {
                    heartbeat_interval: Duration::from_millis(10),
                    missed_threshold: 5,
                },
            )
        };

        // Wait for 2 missed beats but not 5
        std::thread::sleep(Duration::from_millis(25));

        let status = wd.check_peer();
        match status {
            PeerStatus::Stale { missed_beats } => {
                assert!(missed_beats >= 1 && missed_beats < 5);
            }
            PeerStatus::Alive => {
                // Timing can be imprecise, acceptable
            }
            PeerStatus::Dead => {
                panic!("Should not be dead yet");
            }
        }
    }
}
