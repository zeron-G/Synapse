//! Cross-process real communication tests.
//!
//! These tests spawn `synapse_bridge_child` as a child OS process and verify
//! that shared memory, ring buffers, and control-block state all work correctly
//! across process boundaries — not just within a single process.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use synapse_core::*;

/// Path to the child-process helper binary, set by Cargo at test-build time.
const CHILD_EXE: &str = env!("CARGO_BIN_EXE_synapse_bridge_child");

/// Remove any leftover shm file from a previous run (Linux-only; on Windows
/// the kernel object is reference-counted and cleaned up automatically).
fn cleanup(name: &str) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));
    #[cfg(not(unix))]
    let _ = name;
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Poll `host.recv()` for up to `timeout`, returning the first message received.
fn poll_recv(host: &Bridge, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(data) = host.recv() {
            return Some(data);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Host pre-loads a message into the ring, then spawns a child that connects,
/// reads the message, and sends back "ACK:<original>".
/// Verifies real cross-process SPSC ring delivery.
#[test]
fn test_cross_process_echo() {
    let name = "syn_cp_echo";
    cleanup(name);

    let host_bridge = host(name).expect("host failed");

    // Pre-load the message so the ring is ready before the child starts.
    host_bridge.send(b"ping").expect("send failed");

    let mut child = Command::new(CHILD_EXE)
        .args(["connect-echo", name])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn child");

    let reply = poll_recv(&host_bridge, Duration::from_secs(5));

    let status = child.wait().expect("wait failed");
    assert!(status.success(), "child process failed: {status:?}");
    assert_eq!(
        reply.as_deref(),
        Some(b"ACK:ping".as_ref()),
        "expected ACK:ping from child"
    );
}

/// Verifies that the session token stored in the control block is identical
/// when read by a connector in a completely separate OS process.
#[test]
fn test_cross_process_session_token() {
    let name = "syn_cp_session";
    cleanup(name);

    let host_bridge = host(name).expect("host failed");
    let expected_token = host_bridge.session_token();
    assert_ne!(expected_token, 0, "session token must be non-zero");

    // Pre-load message so the child can complete its echo and exit cleanly.
    host_bridge.send(b"check").expect("send failed");

    // Use .output() — blocks until child finishes, captures stdout.
    let output = Command::new(CHILD_EXE)
        .args(["connect-echo", name])
        .output()
        .expect("failed to run child");

    assert!(
        output.status.success(),
        "child failed: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let child_token: u128 = stdout
        .lines()
        .find(|l| l.starts_with("SESSION:"))
        .and_then(|l| l["SESSION:".len()..].parse().ok())
        .expect("no valid SESSION:<token> line in child stdout");

    assert_eq!(
        child_token, expected_token,
        "session token mismatch across processes"
    );
}

/// Host pre-loads N messages; the child connects, echoes each with "ECHO:" prefix,
/// then exits.  Host verifies all N echoes arrive in order.
#[test]
fn test_cross_process_bidirectional() {
    let name = "syn_cp_bidi";
    cleanup(name);

    let host_bridge = host(name).expect("host failed");

    // Pre-load 5 messages.
    let n: u8 = 5;
    for i in 0..n {
        host_bridge.send(&[i]).expect("send failed");
    }

    // Child echoes all 5 then exits.
    let output = Command::new(CHILD_EXE)
        .args(["connect-bidi", name, &n.to_string()])
        .output()
        .expect("failed to run child");

    assert!(
        output.status.success(),
        "child failed: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // All echoes are already in ring_ba; read them in order.
    for i in 0..n {
        let data = host_bridge.recv().unwrap_or_else(|| {
            panic!("expected echo {i} but ring was empty");
        });
        assert!(data.starts_with(b"ECHO:"), "reply should start with ECHO:");
        assert_eq!(data[5..], [i], "echo body mismatch for msg {i}");
    }

    // No more messages.
    assert!(host_bridge.recv().is_none(), "unexpected extra message");
}

/// After a connector attaches in a separate process, the bridge must report
/// `is_ready() == true` on both ends.
#[test]
fn test_cross_process_state_ready() {
    let name = "syn_cp_state";
    cleanup(name);

    let host_bridge = host(name).expect("host failed");
    assert!(
        host_bridge.is_ready(),
        "host must be Ready immediately after creation"
    );

    // Pre-load message so the child echo completes and exits cleanly.
    host_bridge.send(b"state?").expect("send failed");

    let output = Command::new(CHILD_EXE)
        .args(["connect-echo", name])
        .output()
        .expect("failed to run child");

    assert!(output.status.success(), "child failed: {:?}", output.status);

    // Host-side state is still Ready after connector attached and left.
    assert!(host_bridge.is_ready(), "bridge should remain Ready");
}

/// On Unix, a second process attempting to host the same name must fail
/// because `shm_open` uses O_EXCL.  The child binary exits with code 1 and
/// prints "FAILED:<reason>" to stdout.
#[cfg(unix)]
#[test]
fn test_cross_process_double_host() {
    let name = "syn_cp_dhost";
    cleanup(name);

    // Parent holds the host bridge open.
    let _host_bridge = host(name).expect("host failed");

    let output = Command::new(CHILD_EXE)
        .args(["try-host", name])
        .output()
        .expect("failed to run child");

    assert!(
        !output.status.success(),
        "child should have failed to host the same name"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FAILED:"),
        "expected 'FAILED:<reason>' in stdout, got: {stdout}"
    );
}
