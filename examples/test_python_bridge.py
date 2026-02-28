#!/usr/bin/env python3
"""End-to-end tests for the pure-Python SynapseBridge class.

Verifies:
  - Correct host→connector and connector→host delivery
  - Bidirectional messaging via threads
  - Wire-format compatibility with Rust core (magic, version, ring headers)
  - Error cases (connect to non-existent, empty recv)
  - Boundary conditions (max payload, many messages)

Run with:
  python3 examples/test_python_bridge.py

Or via pytest:
  python3 -m pytest examples/test_python_bridge.py -v
"""

import os
import struct
import sys
import threading
import time

# Allow importing SynapseBridge from the sibling python_sender.py.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from python_sender import (  # noqa: E402
    DEFAULT_CAPACITY,
    DEFAULT_SLOT_SIZE,
    MAGIC,
    VERSION,
    SynapseBridge,
)

# ── Utilities ──────────────────────────────────────────────────────────────────


def cleanup(name: str) -> None:
    """Remove any leftover shared memory file (Linux only)."""
    if sys.platform != "win32":
        try:
            os.unlink(f"/dev/shm/{name}")
        except FileNotFoundError:
            pass


# ── Tests ──────────────────────────────────────────────────────────────────────


def test_host_send_connector_recv() -> None:
    """Host sends a message; connector receives it correctly."""
    name = "pybr_send_recv"
    cleanup(name)
    try:
        host = SynapseBridge(name, create=True)
        try:
            conn = SynapseBridge(name, create=False)
            try:
                host.send(b"hello from host")
                received = None
                deadline = time.monotonic() + 2.0
                while time.monotonic() < deadline:
                    received = conn.recv()
                    if received is not None:
                        break
                    time.sleep(0.001)
                assert received == b"hello from host", f"got: {received!r}"
            finally:
                conn.close()
        finally:
            host.destroy()
    finally:
        cleanup(name)


def test_connector_send_host_recv() -> None:
    """Connector sends a message; host receives it correctly."""
    name = "pybr_conn_recv"
    cleanup(name)
    try:
        host = SynapseBridge(name, create=True)
        try:
            conn = SynapseBridge(name, create=False)
            try:
                conn.send(b"hello from connector")
                received = None
                deadline = time.monotonic() + 2.0
                while time.monotonic() < deadline:
                    received = host.recv()
                    if received is not None:
                        break
                    time.sleep(0.001)
                assert received == b"hello from connector", f"got: {received!r}"
            finally:
                conn.close()
        finally:
            host.destroy()
    finally:
        cleanup(name)


def test_bidirectional_threading() -> None:
    """Threaded host + connector exchange N messages bidirectionally."""
    name = "pybr_bidi_thread"
    cleanup(name)
    n = 20
    errors: list = []

    host = SynapseBridge(name, create=True)

    def connector_thread() -> None:
        # Retry connecting until the host has initialised (handles race on Linux).
        conn = None
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            try:
                conn = SynapseBridge(name, create=False)
                break
            except (OSError, FileNotFoundError):
                time.sleep(0.01)
        if conn is None:
            errors.append("connector could not attach within 5 s")
            return
        try:
            received = 0
            deadline2 = time.monotonic() + 5.0
            while received < n and time.monotonic() < deadline2:
                msg = conn.recv()
                if msg is not None:
                    conn.send(b"ACK:" + msg)
                    received += 1
                else:
                    time.sleep(0.002)
            if received != n:
                errors.append(f"connector only got {received}/{n} messages")
        except Exception as exc:  # noqa: BLE001
            errors.append(str(exc))
        finally:
            conn.close()

    t = threading.Thread(target=connector_thread, daemon=True)
    t.start()

    try:
        for i in range(n):
            host.send(f"msg_{i}".encode())

        acks: list = []
        deadline = time.monotonic() + 5.0
        while len(acks) < n and time.monotonic() < deadline:
            msg = host.recv()
            if msg is not None:
                acks.append(msg)
            else:
                time.sleep(0.002)
        t.join(timeout=6.0)
    finally:
        host.destroy()
        cleanup(name)

    assert not errors, f"connector errors: {errors}"
    assert len(acks) == n, f"host only received {len(acks)}/{n} ACKs"
    for i, ack in enumerate(acks):
        expected = f"ACK:msg_{i}".encode()
        assert ack == expected, f"ack[{i}]: {ack!r} != {expected!r}"


def test_wire_format_magic_version() -> None:
    """Python bridge writes MAGIC and VERSION exactly as the Rust core does."""
    name = "pybr_wireformat"
    cleanup(name)
    try:
        bridge = SynapseBridge(name, create=True)
        try:
            buf = bridge._shm
            magic = struct.unpack_from("<Q", buf, 0)[0]
            version = struct.unpack_from("<I", buf, 8)[0]
            state = struct.unpack_from("<I", buf, 72)[0]  # OFF_STATE = 72
            assert magic == MAGIC, f"wrong magic: {magic:#018x} (expected {MAGIC:#018x})"
            assert version == VERSION, f"wrong version: {version}"
            assert state == 1, f"state should be Ready(1), got {state}"
        finally:
            bridge.destroy()
    finally:
        cleanup(name)


def test_wire_format_ring_headers() -> None:
    """Ring header fields (capacity, slot_size, mask) match what Rust expects."""
    name = "pybr_ringheaders"
    cleanup(name)
    cacheline = 64
    try:
        bridge = SynapseBridge(name, create=True)
        try:
            buf = bridge._shm
            for ring_off in (bridge._ab, bridge._ba):
                meta = ring_off + 2 * cacheline
                cap = struct.unpack_from("<Q", buf, meta)[0]
                ss = struct.unpack_from("<Q", buf, meta + 8)[0]
                mask = struct.unpack_from("<Q", buf, meta + 16)[0]
                assert cap == DEFAULT_CAPACITY, f"capacity: {cap}"
                assert ss == DEFAULT_SLOT_SIZE, f"slot_size: {ss}"
                assert mask == DEFAULT_CAPACITY - 1, f"mask: {mask}"
        finally:
            bridge.destroy()
    finally:
        cleanup(name)


def test_connect_nonexistent() -> None:
    """Connecting to a non-existent name raises OSError on Linux.

    Skipped on Windows because Python's mmap(-1, ..., tagname=...) always
    creates a new mapping rather than failing when the name is absent.
    """
    if sys.platform == "win32":
        return  # Windows mmap creates-or-opens; no error expected

    name = "pybr_noexist_xyz999"
    cleanup(name)
    try:
        SynapseBridge(name, create=False)
        raise AssertionError("should have raised FileNotFoundError/OSError")
    except (OSError, FileNotFoundError):
        pass  # expected


def test_empty_recv_returns_none() -> None:
    """recv() on an empty ring returns None without raising."""
    name = "pybr_empty_recv"
    cleanup(name)
    try:
        bridge = SynapseBridge(name, create=True)
        try:
            assert bridge.recv() is None, "empty recv should return None (host side)"
            # Connector side is also empty.
            conn = SynapseBridge(name, create=False)
            try:
                assert conn.recv() is None, "empty recv should return None (connector side)"
            finally:
                conn.close()
        finally:
            bridge.destroy()
    finally:
        cleanup(name)


def test_max_payload() -> None:
    """A payload of exactly slot_size − 4 bytes sends and receives correctly."""
    name = "pybr_maxpayload"
    cleanup(name)
    slot_size = 32  # small slot; max payload = 28 bytes
    try:
        host = SynapseBridge(name, create=True, slot_size=slot_size)
        try:
            conn = SynapseBridge(name, create=False, slot_size=slot_size)
            try:
                payload = b"x" * (slot_size - 4)  # exactly max payload
                host.send(payload)
                received = None
                deadline = time.monotonic() + 2.0
                while time.monotonic() < deadline:
                    received = conn.recv()
                    if received is not None:
                        break
                    time.sleep(0.001)
                assert received == payload, f"got: {received!r}"
            finally:
                conn.close()
        finally:
            host.destroy()
    finally:
        cleanup(name)


def test_multiple_messages() -> None:
    """Send 200 messages in order; verify all arrive without loss or reordering."""
    name = "pybr_multi_msg"
    cleanup(name)
    n = 200
    try:
        host = SynapseBridge(name, create=True)
        try:
            conn = SynapseBridge(name, create=False)
            try:
                for i in range(n):
                    host.send(i.to_bytes(4, "little"))

                received: list = []
                deadline = time.monotonic() + 5.0
                while len(received) < n and time.monotonic() < deadline:
                    msg = conn.recv()
                    if msg is not None:
                        received.append(msg)
                    else:
                        time.sleep(0.001)

                assert len(received) == n, f"got {len(received)}/{n} messages"
                for i, msg in enumerate(received):
                    expected = i.to_bytes(4, "little")
                    assert msg == expected, f"msg[{i}]: {msg!r} != {expected!r}"
            finally:
                conn.close()
        finally:
            host.destroy()
    finally:
        cleanup(name)


# ── Test runner ────────────────────────────────────────────────────────────────

TESTS = [
    test_host_send_connector_recv,
    test_connector_send_host_recv,
    test_bidirectional_threading,
    test_wire_format_magic_version,
    test_wire_format_ring_headers,
    test_connect_nonexistent,
    test_empty_recv_returns_none,
    test_max_payload,
    test_multiple_messages,
]

if __name__ == "__main__":
    print("=" * 60)
    print("  Synapse Python Bridge Tests")
    print("=" * 60)
    failed: list = []
    for test_fn in TESTS:
        try:
            test_fn()
            print(f"  PASS  {test_fn.__name__}")
        except Exception as exc:  # noqa: BLE001
            print(f"  FAIL  {test_fn.__name__}: {exc}")
            failed.append(test_fn.__name__)

    print()
    print("=" * 60)
    if failed:
        print(f"  FAILED {len(failed)}/{len(TESTS)}")
        for fn_name in failed:
            print(f"    x {fn_name}")
        print("=" * 60)
        sys.exit(1)
    else:
        print(f"  All {len(TESTS)} tests passed")
        print("=" * 60)
