//! Full pipeline integration tests for Phase 2 features.
//!
//! Tests all new features together: TypedChannels + LatestSlots + AdaptiveWait + Shutdown.
//! Includes stress tests with concurrent channels, multiple readers/writers,
//! and sustained throughput.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use synapse_core::control::{ControlBlock, State};
use synapse_core::latest_slot::LatestSlot;
use synapse_core::ring::RingHeader;
use synapse_core::shutdown::{
    PeerStatus, ShutdownMode, ShutdownProtocol, Watchdog, WatchdogConfig,
};
use synapse_core::typed_channel::{ChannelRegistry, TypedChannel, REGISTRY_SIZE};
use synapse_core::wait::{WaitStrategy, Waiter};

// ── Test types ──

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
    position: Vec3f,
    velocity: Vec3f,
    health: f32,
    frame_id: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
struct Command {
    tag: u32,
    target_x: f32,
    target_y: f32,
    target_z: f32,
}

// ── Helpers ──

#[repr(C, align(64))]
struct AlignedBuf<const N: usize>([u8; N]);

fn alloc_typed_ring<T: Copy>(capacity: u64) -> (Vec<u8>, TypedChannel<T>) {
    let type_size = std::mem::size_of::<T>();
    let slot_size = ((4 + type_size + 7) & !7) as u64;
    let size = RingHeader::region_size(capacity, slot_size);
    let mut region = vec![0u8; size];
    unsafe {
        RingHeader::init(region.as_mut_ptr(), capacity, slot_size);
        let ch = TypedChannel::<T>::from_ring_ptr(region.as_mut_ptr()).unwrap();
        (region, ch)
    }
}

// ── Integration tests ──

/// Test TypedChannel + LatestSlot working together in a simulated game loop.
#[test]
fn test_typed_channel_with_latest_slot() {
    // Allocate a typed channel for streaming commands
    let (_ring_mem, cmd_channel) = alloc_typed_ring::<Command>(64);

    // Allocate a latest-value slot for game state
    let lvs_size = LatestSlot::<GameState>::required_size();
    let mut lvs_mem = vec![0u8; lvs_size];
    unsafe {
        LatestSlot::<GameState>::init(lvs_mem.as_mut_ptr());
    }
    let state_slot = unsafe { LatestSlot::<GameState>::from_ptr(lvs_mem.as_mut_ptr()) };

    // Simulate: game loop writes state, AI reads state and sends commands
    // Use batches that fit within the ring capacity (64)
    let total_frames = 64u64;
    for frame in 0..total_frames {
        let state = GameState {
            position: Vec3f {
                x: frame as f32,
                y: 0.0,
                z: 0.0,
            },
            velocity: Vec3f {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
            health: 100.0 - frame as f32 * 0.5,
            frame_id: frame,
        };
        state_slot.write(&state);

        // AI reads latest state and sends a command
        let latest = state_slot.read().unwrap();
        assert_eq!(latest.frame_id, frame);

        let cmd = Command {
            tag: 1,
            target_x: latest.position.x + 10.0,
            target_y: 0.0,
            target_z: 0.0,
        };
        cmd_channel.write(&cmd).unwrap();
    }

    // Drain all commands
    let mut count = 0u64;
    while cmd_channel.read().is_some() {
        count += 1;
    }
    assert_eq!(count, total_frames);
}

/// Test AdaptiveWait with TypedChannel for blocking receive.
#[test]
fn test_adaptive_wait_with_typed_channel() {
    let capacity = 64u64;
    let type_size = std::mem::size_of::<GameState>();
    let slot_size = ((4 + type_size + 7) & !7) as u64;
    let ring_size = RingHeader::region_size(capacity, slot_size);
    let mem = Arc::new(vec![0u8; ring_size]);

    unsafe {
        RingHeader::init(mem.as_ptr() as *mut u8, capacity, slot_size);
    }

    let mem_writer = Arc::clone(&mem);
    let mem_reader = Arc::clone(&mem);

    let flag = Arc::new(AtomicU32::new(0));
    let flag_writer = Arc::clone(&flag);
    let flag_reader = Arc::clone(&flag);

    // Writer thread: writes a value after a delay
    let writer = thread::spawn(move || {
        let ch = unsafe {
            TypedChannel::<GameState>::from_ring_ptr(mem_writer.as_ptr() as *mut u8).unwrap()
        };
        thread::sleep(Duration::from_millis(50));
        let state = GameState {
            position: Vec3f {
                x: 42.0,
                y: 0.0,
                z: 0.0,
            },
            velocity: Vec3f {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            health: 100.0,
            frame_id: 999,
        };
        ch.write(&state).unwrap();
        flag_writer.store(1, Ordering::Release);
        synapse_core::wait::wake_one(&flag_writer);
    });

    // Reader thread: uses adaptive wait
    let reader = thread::spawn(move || {
        let ch = unsafe {
            TypedChannel::<GameState>::from_ring_ptr(mem_reader.as_ptr() as *mut u8).unwrap()
        };
        let waiter = Waiter::new(WaitStrategy::Adaptive {
            spin_count: 50,
            yield_count: 10,
        });

        let found = waiter.wait_until(&flag_reader, 0, || !ch.is_empty(), Duration::from_secs(5));
        assert!(found, "adaptive wait should detect data");

        let state = ch.read().unwrap();
        assert_eq!(state.frame_id, 999);
        assert_eq!(state.position.x, 42.0);
    });

    writer.join().unwrap();
    reader.join().unwrap();
}

/// Test Shutdown + Watchdog together.
#[test]
fn test_shutdown_with_watchdog() {
    let mut mem = Box::new(AlignedBuf::<256>([0u8; 256]));
    unsafe {
        ControlBlock::init(mem.0.as_mut_ptr(), 256, 0xDEAD);
    }
    let cb = unsafe { &*(mem.0.as_ptr() as *const ControlBlock) };
    cb.set_state(State::Ready);

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

    // Both beat regularly
    host_wd.beat();
    conn_wd.beat();
    assert_eq!(host_wd.check_peer(), PeerStatus::Alive);
    assert_eq!(conn_wd.check_peer(), PeerStatus::Alive);

    // Host initiates graceful shutdown
    let proto = unsafe { ShutdownProtocol::new(mem.0.as_ptr() as *mut ControlBlock, true) };
    proto.initiate(ShutdownMode::Graceful);

    assert_eq!(cb.state(), State::Closing);

    // Complete shutdown
    proto.complete();
    assert_eq!(cb.state(), State::Dead);

    // Watchdog should detect dead state
    assert_eq!(host_wd.check_peer(), PeerStatus::Dead);
}

/// Stress test: multiple typed channels with concurrent push/pop.
#[test]
fn test_concurrent_typed_channels() {
    let iterations = 5_000u64;
    let capacity = 256u64;

    // Channel 1: GameState
    let gs_type_size = std::mem::size_of::<GameState>();
    let gs_slot = ((4 + gs_type_size + 7) & !7) as u64;
    let gs_ring_size = RingHeader::region_size(capacity, gs_slot);
    let gs_mem = Arc::new(vec![0u8; gs_ring_size]);
    unsafe {
        RingHeader::init(gs_mem.as_ptr() as *mut u8, capacity, gs_slot);
    }

    // Channel 2: Command
    let cmd_type_size = std::mem::size_of::<Command>();
    let cmd_slot = ((4 + cmd_type_size + 7) & !7) as u64;
    let cmd_ring_size = RingHeader::region_size(capacity, cmd_slot);
    let cmd_mem = Arc::new(vec![0u8; cmd_ring_size]);
    unsafe {
        RingHeader::init(cmd_mem.as_ptr() as *mut u8, capacity, cmd_slot);
    }

    let gs_mem_w = Arc::clone(&gs_mem);
    let gs_mem_r = Arc::clone(&gs_mem);
    let cmd_mem_w = Arc::clone(&cmd_mem);
    let cmd_mem_r = Arc::clone(&cmd_mem);

    // Writer thread: writes game states and commands
    let writer = thread::spawn(move || {
        let gs_ch = unsafe {
            TypedChannel::<GameState>::from_ring_ptr(gs_mem_w.as_ptr() as *mut u8).unwrap()
        };
        let cmd_ch = unsafe {
            TypedChannel::<Command>::from_ring_ptr(cmd_mem_w.as_ptr() as *mut u8).unwrap()
        };

        for i in 0..iterations {
            let state = GameState {
                position: Vec3f {
                    x: i as f32,
                    y: 0.0,
                    z: 0.0,
                },
                velocity: Vec3f {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                health: 100.0,
                frame_id: i,
            };

            // Wait if ring is full
            loop {
                match gs_ch.write(&state) {
                    Ok(()) => break,
                    Err(synapse_core::error::SynapseError::RingFull) => {
                        std::hint::spin_loop();
                    }
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }

            let cmd = Command {
                tag: (i % 3) as u32,
                target_x: i as f32,
                target_y: 0.0,
                target_z: 0.0,
            };
            loop {
                match cmd_ch.write(&cmd) {
                    Ok(()) => break,
                    Err(synapse_core::error::SynapseError::RingFull) => {
                        std::hint::spin_loop();
                    }
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }
        }
    });

    // Reader thread: reads from both channels
    let reader = thread::spawn(move || {
        let gs_ch = unsafe {
            TypedChannel::<GameState>::from_ring_ptr(gs_mem_r.as_ptr() as *mut u8).unwrap()
        };
        let cmd_ch = unsafe {
            TypedChannel::<Command>::from_ring_ptr(cmd_mem_r.as_ptr() as *mut u8).unwrap()
        };

        let mut gs_count = 0u64;
        let mut cmd_count = 0u64;
        let mut last_frame = 0u64;

        while gs_count < iterations || cmd_count < iterations {
            if let Some(state) = gs_ch.read() {
                assert!(state.frame_id >= last_frame || last_frame == 0);
                last_frame = state.frame_id;
                gs_count += 1;
            }
            if let Some(_cmd) = cmd_ch.read() {
                cmd_count += 1;
            }
            if gs_count < iterations || cmd_count < iterations {
                std::hint::spin_loop();
            }
        }

        (gs_count, cmd_count)
    });

    writer.join().unwrap();
    let (gs, cmd) = reader.join().unwrap();
    assert_eq!(gs, iterations);
    assert_eq!(cmd, iterations);
}

/// Stress test: LatestSlot under sustained write pressure with multiple readers.
#[test]
fn test_lvs_sustained_pressure() {
    let iterations = 20_000u64;
    let num_readers = 4;

    let size = LatestSlot::<GameState>::required_size();
    let mem = Arc::new(vec![0u8; size]);
    unsafe {
        LatestSlot::<GameState>::init(mem.as_ptr() as *mut u8);
    }

    let started = Arc::new(AtomicU32::new(0));

    // Start readers FIRST so they overlap with the writer
    let readers: Vec<_> = (0..num_readers)
        .map(|_| {
            let mem_r = Arc::clone(&mem);
            let started_r = Arc::clone(&started);
            thread::spawn(move || {
                let slot = unsafe { LatestSlot::<GameState>::from_ptr(mem_r.as_ptr() as *mut u8) };
                started_r.fetch_add(1, Ordering::Release);
                let mut consistent = 0u64;
                for _ in 0..iterations * 10 {
                    if let Some(state) = slot.read() {
                        assert_eq!(
                            state.position.y,
                            state.position.x * 2.0,
                            "inconsistent y at frame {}",
                            state.frame_id
                        );
                        assert_eq!(
                            state.position.z,
                            state.position.x * 3.0,
                            "inconsistent z at frame {}",
                            state.frame_id
                        );
                        consistent += 1;
                    }
                    std::hint::spin_loop();
                }
                consistent
            })
        })
        .collect();

    // Wait for readers to be ready
    while started.load(Ordering::Acquire) < num_readers as u32 {
        std::hint::spin_loop();
    }

    let mem_w = Arc::clone(&mem);
    let writer = thread::spawn(move || {
        let slot = unsafe { LatestSlot::<GameState>::from_ptr(mem_w.as_ptr() as *mut u8) };
        for i in 0..iterations {
            let state = GameState {
                position: Vec3f {
                    x: i as f32,
                    y: i as f32 * 2.0,
                    z: i as f32 * 3.0,
                },
                velocity: Vec3f {
                    x: 1.0,
                    y: 0.0,
                    z: 0.0,
                },
                health: 100.0 - (i % 100) as f32,
                frame_id: i,
            };
            slot.write(&state);
        }
    });

    writer.join().unwrap();
    for r in readers {
        let reads = r.join().unwrap();
        assert!(
            reads > 0,
            "each reader should get at least one consistent read"
        );
    }
}

/// Test ChannelRegistry + TypedChannel end-to-end with multiple channels.
#[test]
fn test_registry_multi_channel_pipeline() {
    let capacity = 32u64;

    // Compute sizes
    let gs_type_size = std::mem::size_of::<GameState>();
    let gs_slot = ((4 + gs_type_size + 7) & !7) as u64;
    let cmd_type_size = std::mem::size_of::<Command>();
    let cmd_slot = ((4 + cmd_type_size + 7) & !7) as u64;

    let gs_ring_size = RingHeader::region_size(capacity, gs_slot);
    let cmd_ring_size = RingHeader::region_size(capacity, cmd_slot);

    let total = REGISTRY_SIZE + gs_ring_size + cmd_ring_size;
    let mut mem = vec![0u8; total];

    unsafe {
        // Init registry
        ChannelRegistry::init(mem.as_mut_ptr());
        let reg = ChannelRegistry::from_ptr(mem.as_mut_ptr());

        // Init rings
        let gs_offset = REGISTRY_SIZE;
        let cmd_offset = REGISTRY_SIZE + gs_ring_size;
        RingHeader::init(mem.as_mut_ptr().add(gs_offset), capacity, gs_slot);
        RingHeader::init(mem.as_mut_ptr().add(cmd_offset), capacity, cmd_slot);

        // Register channels
        reg.register("game_state", gs_offset as u64, capacity, gs_slot)
            .unwrap();
        reg.register("commands", cmd_offset as u64, capacity, cmd_slot)
            .unwrap();

        // Look up and create typed channels
        let gs_desc = reg.lookup("game_state").unwrap();
        let cmd_desc = reg.lookup("commands").unwrap();

        let gs_ch =
            TypedChannel::<GameState>::from_ring_ptr(mem.as_mut_ptr().add(gs_desc.offset)).unwrap();
        let cmd_ch =
            TypedChannel::<Command>::from_ring_ptr(mem.as_mut_ptr().add(cmd_desc.offset)).unwrap();

        // Pipeline: write states, write commands, read both
        for i in 0..capacity as u32 {
            gs_ch
                .write(&GameState {
                    position: Vec3f {
                        x: i as f32,
                        y: 0.0,
                        z: 0.0,
                    },
                    velocity: Vec3f {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    health: 100.0,
                    frame_id: i as u64,
                })
                .unwrap();

            cmd_ch
                .write(&Command {
                    tag: i % 3,
                    target_x: i as f32,
                    target_y: 0.0,
                    target_z: 0.0,
                })
                .unwrap();
        }

        // Read and verify order
        for i in 0..capacity as u32 {
            let state = gs_ch.read().unwrap();
            assert_eq!(state.frame_id, i as u64);

            let cmd = cmd_ch.read().unwrap();
            assert_eq!(cmd.tag, i % 3);
        }

        // Both channels should be empty
        assert!(gs_ch.is_empty());
        assert!(cmd_ch.is_empty());

        // Registry should list both channels
        let channels = reg.channels();
        assert_eq!(channels.len(), 2);
    }
}

/// Test the full lifecycle: create → use → shutdown → cleanup.
#[test]
fn test_full_lifecycle_with_bridge() {
    let name = "synapse_test_lifecycle";
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));

    // Host creates bridge
    let h = synapse_core::host(name).expect("host failed");
    let c = synapse_core::connect(name).expect("connect failed");

    // Verify ready state
    assert!(h.is_ready());
    assert!(c.is_ready());
    assert_eq!(h.session_token(), c.session_token());

    // Send messages in both directions
    for i in 0..50u32 {
        h.send(format!("frame_{i}").as_bytes()).unwrap();
    }
    for i in 0..50u32 {
        let data = c.recv().unwrap();
        assert_eq!(data, format!("frame_{i}").as_bytes());
    }

    for i in 0..30u32 {
        c.send(format!("cmd_{i}").as_bytes()).unwrap();
    }
    for i in 0..30u32 {
        let data = h.recv().unwrap();
        assert_eq!(data, format!("cmd_{i}").as_bytes());
    }

    // Drop both sides (cleanup)
    drop(h);
    drop(c);
}

/// Stress test: sustained throughput with backpressure.
#[test]
fn test_sustained_throughput_backpressure() {
    let name = "synapse_test_throughput";
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));

    let h = synapse_core::host(name).expect("host failed");
    let c = synapse_core::connect(name).expect("connect failed");

    let total_messages = 10_000u32;
    let payload = vec![0xABu8; 200]; // 200 bytes per message

    // Push until full, then pop some, then push more — tests backpressure
    let mut sent = 0u32;
    let mut received = 0u32;

    while received < total_messages {
        // Try to send a batch
        while sent < total_messages {
            match h.send(&payload) {
                Ok(()) => sent += 1,
                Err(synapse_core::error::SynapseError::RingFull) => break,
                Err(e) => panic!("send error: {e:?}"),
            }
        }

        // Drain available messages
        while let Some(_data) = c.recv() {
            received += 1;
        }
    }

    assert_eq!(received, total_messages);

    drop(h);
    drop(c);
}
