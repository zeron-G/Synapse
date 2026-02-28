//! Child-process helper binary for cross-process integration tests.
//!
//! Usage:
//!   synapse_bridge_child connect-echo <name>
//!     Connect to an existing bridge, print the session token to stdout,
//!     wait up to 5 s for one message, echo it back with "ACK:" prefix, then exit 0.
//!
//!   synapse_bridge_child connect-bidi <name> <n>
//!     Connect, receive exactly <n> messages, echo each back with "ECHO:" prefix.
//!
//!   synapse_bridge_child try-host <name>
//!     Attempt to create a host bridge.  Prints "HOSTED" and exits 0 on success,
//!     prints "FAILED:<reason>" and exits 1 on failure.

use std::time::{Duration, Instant};
use synapse_core::{connect, host};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: synapse_bridge_child <command> <name> [extra...]");
        std::process::exit(1);
    }

    let command = args[1].as_str();
    let name = args[2].as_str();

    match command {
        "connect-echo" => cmd_connect_echo(name),
        "connect-bidi" => {
            let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            cmd_connect_bidi(name, n);
        }
        "try-host" => cmd_try_host(name),
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(1);
        }
    }
}

/// Connect to bridge, print session token, wait for one message, echo it back.
fn cmd_connect_echo(name: &str) {
    let bridge = match connect(name) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };

    // Print session token so the parent can verify it matches.
    println!("SESSION:{}", bridge.session_token());

    // Wait up to 5 s for a message.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(data) = bridge.recv() {
            let mut reply = b"ACK:".to_vec();
            reply.extend_from_slice(&data);
            if let Err(e) = bridge.send(&reply) {
                eprintln!("send ACK failed: {e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        if Instant::now() > deadline {
            eprintln!("timeout waiting for message");
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Connect to bridge, receive exactly `n` messages, echo each with "ECHO:" prefix.
fn cmd_connect_bidi(name: &str, n: usize) {
    let bridge = match connect(name) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };

    for _ in 0..n {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(data) = bridge.recv() {
                let mut reply = b"ECHO:".to_vec();
                reply.extend_from_slice(&data);
                if let Err(e) = bridge.send(&reply) {
                    eprintln!("send ECHO failed: {e}");
                    std::process::exit(1);
                }
                break;
            }
            if Instant::now() > deadline {
                eprintln!("timeout waiting for message");
                std::process::exit(2);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Attempt to host a bridge with the given name.
fn cmd_try_host(name: &str) {
    match host(name) {
        Ok(_bridge) => {
            println!("HOSTED");
            std::process::exit(0);
        }
        Err(e) => {
            println!("FAILED:{e}");
            std::process::exit(1);
        }
    }
}
