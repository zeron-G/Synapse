use synapse_core::error::SynapseError;
use synapse_core::ring::{Ring, RingHeader};
use synapse_core::*;

#[test]
fn test_control_block_validation() {
    let name = "syn_itest_cb";
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));

    let h = host(name).unwrap();
    let c = connect(name).unwrap();
    assert!(c.is_ready());
    assert_eq!(h.session_token(), c.session_token());
}

#[test]
fn test_ring_data_too_large() {
    let capacity: u64 = 4;
    let slot_size: u64 = 16;
    let size = RingHeader::region_size(capacity, slot_size);
    let mut region = vec![0u8; size];
    unsafe {
        RingHeader::init(region.as_mut_ptr(), capacity, slot_size);
    }
    let ring = unsafe { Ring::from_ptr(region.as_mut_ptr()) };

    ring.try_push(&[0u8; 12]).unwrap();
    assert!(matches!(
        ring.try_push(&[0u8; 13]),
        Err(SynapseError::DataTooLarge { .. })
    ));
}

#[test]
fn test_empty_recv_returns_none() {
    let name = "syn_itest_empty";
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));
    let h = host(name).unwrap();
    let c = connect(name).unwrap();
    assert!(h.recv().is_none());
    assert!(c.recv().is_none());
}

#[test]
fn test_high_throughput() {
    let name = "syn_itest_tp";
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));
    let h = host(name).unwrap();
    let c = connect(name).unwrap();

    for i in 0..1000u32 {
        h.send(&i.to_le_bytes()).unwrap();
    }
    for i in 0..1000u32 {
        let data = c.recv().unwrap();
        assert_eq!(data, i.to_le_bytes());
    }
}
