//! Error path tests for Synapse.
//!
//! Verifies that every documented error variant (`ShmError`, `BadMagic`,
//! `VersionMismatch`, `DataTooLarge`, `RingFull`) is returned under the
//! correct conditions, and that a double-host attempt fails on Unix.

use synapse_core::control::{MAGIC, VERSION};
use synapse_core::error::SynapseError;
use synapse_core::ring::{Ring, RingHeader};
use synapse_core::shm::SharedRegion;
use synapse_core::*;

/// Remove any leftover shm from a previous run (Linux only).
fn cleanup(name: &str) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));
    // On Windows, kernel objects are reference-counted and auto-cleaned.
    #[cfg(not(unix))]
    let _ = name;
}

/// Total shm size for the default capacity/slot_size config.
/// Mirrors the private `region_size` function in lib.rs.
fn default_total_size() -> usize {
    let ring_size = RingHeader::region_size(DEFAULT_CAPACITY, DEFAULT_SLOT_SIZE);
    256 + ring_size * 2 // 256 == CONTROL_SIZE
}

// ── ShmError: connecting to a non-existent region ────────────────────────────

#[test]
fn test_connect_nonexistent() {
    // Use a name that is guaranteed never to exist.
    let name = "syn_err_noexist_42xyz";
    cleanup(name);

    // `connect()` returns Result<Bridge, SynapseError>; Bridge doesn't impl
    // Debug so we extract the error with `.err().expect()` instead of
    // `.expect_err()`, which would require T: Debug.
    let err = connect(name)
        .err()
        .expect("connect should fail for non-existent shm");
    assert!(
        matches!(err, SynapseError::ShmError(_)),
        "expected ShmError, got: {err:?}"
    );
}

// ── BadMagic: zeroed region has magic == 0 ───────────────────────────────────

#[test]
fn test_bad_magic() {
    let name = "syn_err_badmagic";
    cleanup(name);

    // `SharedRegion::create` zeroes the region — magic defaults to 0 ≠ MAGIC.
    let size = default_total_size();
    let _region = SharedRegion::create(name, size).expect("create failed");

    let err = connect(name)
        .err()
        .expect("connect should fail with BadMagic");
    assert!(
        matches!(err, SynapseError::BadMagic { .. }),
        "expected BadMagic, got: {err:?}"
    );
    // _region is dropped here: shm_unlink on Linux, CloseHandle on Windows.
}

// ── VersionMismatch: correct magic, wrong version ────────────────────────────

#[test]
fn test_version_mismatch() {
    let name = "syn_err_version";
    cleanup(name);

    let size = default_total_size();
    let region = SharedRegion::create(name, size).expect("create failed");

    // Write the correct magic then a bumped version number.
    unsafe {
        let ptr = region.as_ptr();
        (ptr as *mut u64).write(MAGIC); // offset 0: magic (u64)
        (ptr.add(8) as *mut u32).write(VERSION + 1); // offset 8: version (u32)
    }

    let err = connect(name)
        .err()
        .expect("connect should fail with VersionMismatch");
    assert!(
        matches!(err, SynapseError::VersionMismatch { .. }),
        "expected VersionMismatch, got: {err:?}"
    );
}

// ── DataTooLarge: payload exceeds slot capacity ───────────────────────────────

#[test]
fn test_data_too_large() {
    let name = "syn_err_toolarge";
    cleanup(name);

    // slot_size = 16 → max payload = 12 bytes (16 − 4-byte length prefix)
    let h = host_with_config(name, 4, 16).expect("host failed");
    let big = vec![0u8; 13]; // 13 > 12

    // `send` returns Result<(), SynapseError>; () is Debug so expect_err is fine.
    let err = h.send(&big).expect_err("should fail with DataTooLarge");
    assert!(
        matches!(err, SynapseError::DataTooLarge { .. }),
        "expected DataTooLarge, got: {err:?}"
    );
}

// ── RingFull via Bridge ───────────────────────────────────────────────────────

#[test]
fn test_ring_full_bridge() {
    let name = "syn_err_ringfull";
    cleanup(name);

    // capacity = 4, slot_size = 32 → ring_ab holds exactly 4 messages
    let h = host_with_config(name, 4, 32).expect("host failed");
    let _c = connect_with_config(name, 4, 32).expect("connect failed");

    for _ in 0..4 {
        h.send(b"data").expect("should not be full yet");
    }
    let err = h.send(b"overflow").expect_err("ring should be full");
    assert_eq!(
        err,
        SynapseError::RingFull,
        "expected RingFull, got: {err:?}"
    );
}

// ── RingFull via Ring directly ────────────────────────────────────────────────

#[test]
fn test_ring_full_direct() {
    let capacity: u64 = 4;
    let slot_size: u64 = 16;
    let size = RingHeader::region_size(capacity, slot_size);
    let mut region = vec![0u8; size];
    unsafe { RingHeader::init(region.as_mut_ptr(), capacity, slot_size) };
    let ring = unsafe { Ring::from_ptr(region.as_mut_ptr()) };

    for _ in 0..capacity {
        ring.try_push(b"x")
            .expect("should succeed while capacity remains");
    }
    let err = ring.try_push(b"x").expect_err("ring should be full");
    assert_eq!(err, SynapseError::RingFull, "expected RingFull");
}

// ── Double host (same process) — Unix only ────────────────────────────────────

#[cfg(unix)]
#[test]
fn test_double_host_same_process() {
    let name = "syn_err_dblhost";
    cleanup(name);

    let _first = host(name).expect("first host must succeed");
    // O_EXCL prevents a second creator while the first is alive.
    let err = host(name).err().expect("second host must fail");
    assert!(
        matches!(err, SynapseError::ShmError(_)),
        "expected ShmError for double-host, got: {err:?}"
    );
}
