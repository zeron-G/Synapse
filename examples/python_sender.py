#!/usr/bin/env python3
"""Synapse Example: Python sender (host side) — pure mmap.

Sends 10 frames bidirectionally with a C++ receiver and expects ACK replies.
Run this FIRST, then start cpp_receiver in another terminal.

Build C++ side (from examples/ directory):
  Linux:   g++ -std=c++17 -O2 -I../bindings/cpp/include -lrt -o cpp_receiver cpp_receiver.cpp
  Windows: g++ -std=c++17 -O2 -I../bindings/cpp/include -o cpp_receiver.exe cpp_receiver.cpp

Usage:
  python3 examples/python_sender.py
"""

import sys
import os
import mmap
import struct
import time

# ── Protocol constants — must match Rust core (control.rs, ring.rs) and synapse.h ─────────────

MAGIC             = 0x53594E4150534500   # "SYNAPSE\0" little-endian
VERSION           = 1
CONTROL_SIZE      = 256                  # bytes reserved for ControlBlock
CACHELINE         = 64
RING_HEADER_SIZE  = 3 * CACHELINE        # head(64) + tail(64) + meta(64) = 192 bytes
DEFAULT_CAPACITY  = 1024                 # ring slots (power-of-2)
DEFAULT_SLOT_SIZE = 256                  # bytes per slot: 4-byte length prefix + payload

# ControlBlock field offsets — derived from control.rs #[repr(C, align(64))]:
#   magic(u64)@0  version(u32)@8  flags(u32)@12  region_size(u64)@16
#   creator_pid(u64)@24  connector_pid(u64)@32
#   session_token_lo(u64)@40  session_token_hi(u64)@48
#   creator_heartbeat(AtomicU64)@56  connector_heartbeat(AtomicU64)@64
#   state(AtomicU32)@72  channel_count(u32)@76  _reserved[128]@80
OFF_MAGIC        = 0    # u64
OFF_VERSION      = 8    # u32
OFF_REGION_SIZE  = 16   # u64
OFF_STATE        = 72   # AtomicU32  (0=Init, 1=Ready, 2=Closing, 3=Dead)

# Channel name — must match the name passed to synapse::connect() in cpp_receiver.
# On Windows the Rust/C++ bridge opens "Local\synapse_{name}"; Python mmap uses
# the same tagname so both sides map the identical kernel object.
CHANNEL_NAME = "demo"


# ── Size helpers ───────────────────────────────────────────────────────────────────────────────

def _ring_region_size(capacity: int, slot_size: int) -> int:
    return RING_HEADER_SIZE + capacity * slot_size


def _total_size(capacity: int, slot_size: int) -> int:
    return CONTROL_SIZE + _ring_region_size(capacity, slot_size) * 2


# ── Bridge ─────────────────────────────────────────────────────────────────────────────────────

class SynapseBridge:
    """Pure-Python Synapse bridge backed by raw mmap (no multiprocessing module).

    Layout (offsets from base of shared region):
      [0 .. 256)           ControlBlock
      [256 .. 256+R)       ring_ab  — host→connector  (A→B)
      [256+R .. 256+2*R)   ring_ba  — connector→host  (B→A)
    where R = _ring_region_size(capacity, slot_size).
    """

    def __init__(self, name: str, create: bool = False,
                 capacity: int = DEFAULT_CAPACITY,
                 slot_size: int = DEFAULT_SLOT_SIZE):
        self._name  = name
        self._cap   = capacity
        self._ss    = slot_size
        self._mask  = capacity - 1
        self._tot   = _total_size(capacity, slot_size)
        self._ab    = CONTROL_SIZE                                       # ring_ab offset
        self._ba    = CONTROL_SIZE + _ring_region_size(capacity, slot_size)  # ring_ba offset
        self._host  = create

        self._shm = self._map(name, self._tot, create)

        if create:
            self._init_region()
        else:
            self._validate_region()

    # ── Platform-specific mmap ─────────────────────────────────────────────

    @staticmethod
    def _map(name: str, size: int, create: bool):
        """Open (and optionally create) a named shared memory region via mmap.

        Naming convention mirrors Rust/C++ shm.rs:
          Windows → "Local\\synapse_{name}"  (kernel named section)
          Linux   → /dev/shm/{name}          (POSIX shm_open path)
        """
        if sys.platform == "win32":
            tagname = f"Local\\synapse_{name}"
            # mmap(-1, ...) uses CreateFileMappingW internally; tagname is the object name.
            # Creates a new mapping if it doesn't exist, or opens existing one.
            return mmap.mmap(-1, size, tagname=tagname, access=mmap.ACCESS_WRITE)
        else:
            path = f"/dev/shm/{name}"
            if create:
                fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o660)
                os.ftruncate(fd, size)
            else:
                fd = os.open(path, os.O_RDWR)
            shm = mmap.mmap(fd, size)
            os.close(fd)
            return shm

    # ── Initialisation ─────────────────────────────────────────────────────

    def _init_region(self) -> None:
        """Zero the region, write ControlBlock fields, and initialise ring headers."""
        buf = self._shm

        # Zero entire region (handles stale data from a previous run)
        buf.seek(0)
        buf.write(b"\x00" * self._tot)

        # ControlBlock
        struct.pack_into("<Q", buf, OFF_MAGIC,       MAGIC)
        struct.pack_into("<I", buf, OFF_VERSION,     VERSION)
        struct.pack_into("<Q", buf, OFF_REGION_SIZE, self._tot)

        # Ring headers: capacity, slot_size, mask at cacheline 2 (offset 128 within ring)
        for ring_off in (self._ab, self._ba):
            struct.pack_into("<Q", buf, ring_off + 2 * CACHELINE,      self._cap)
            struct.pack_into("<Q", buf, ring_off + 2 * CACHELINE + 8,  self._ss)
            struct.pack_into("<Q", buf, ring_off + 2 * CACHELINE + 16, self._mask)

        # State = Ready (1) — connector may attach
        struct.pack_into("<I", buf, OFF_STATE, 1)

    def _validate_region(self) -> None:
        """Basic sanity check on magic and version when connecting."""
        magic = struct.unpack_from("<Q", self._shm, OFF_MAGIC)[0]
        if magic != MAGIC:
            raise RuntimeError(f"Bad magic: {magic:#018x} (expected {MAGIC:#018x})")
        ver = struct.unpack_from("<I", self._shm, OFF_VERSION)[0]
        if ver != VERSION:
            raise RuntimeError(f"Version mismatch: got {ver}, expected {VERSION}")

    # ── Ring operations ────────────────────────────────────────────────────

    def _push(self, ring_off: int, data: bytes) -> None:
        buf  = self._shm
        head = struct.unpack_from("<Q", buf, ring_off)[0]
        tail = struct.unpack_from("<Q", buf, ring_off + CACHELINE)[0]
        if head - tail >= self._cap:
            raise BufferError("ring full")
        slot = ring_off + RING_HEADER_SIZE + (head & self._mask) * self._ss
        struct.pack_into("<I", buf, slot, len(data))
        buf[slot + 4 : slot + 4 + len(data)] = data
        struct.pack_into("<Q", buf, ring_off, head + 1)

    def _pop(self, ring_off: int):
        buf  = self._shm
        tail = struct.unpack_from("<Q", buf, ring_off + CACHELINE)[0]
        head = struct.unpack_from("<Q", buf, ring_off)[0]
        if head == tail:
            return None
        slot   = ring_off + RING_HEADER_SIZE + (tail & self._mask) * self._ss
        length = struct.unpack_from("<I", buf, slot)[0]
        data   = bytes(buf[slot + 4 : slot + 4 + length])
        struct.pack_into("<Q", buf, ring_off + CACHELINE, tail + 1)
        return data

    # ── Public API ─────────────────────────────────────────────────────────

    def send(self, data: bytes) -> None:
        """Host pushes to ring_ab (A→B); connector pushes to ring_ba (B→A)."""
        self._push(self._ab if self._host else self._ba, data)

    def recv(self):
        """Host pops from ring_ba (B→A); connector pops from ring_ab (A→B).

        Returns bytes or None if the ring is empty.
        """
        return self._pop(self._ba if self._host else self._ab)

    # ── Lifecycle ──────────────────────────────────────────────────────────

    def close(self) -> None:
        self._shm.close()

    def destroy(self) -> None:
        """Close and (on Linux) unlink the backing shm file."""
        self._shm.close()
        if sys.platform != "win32":
            try:
                os.unlink(f"/dev/shm/{self._name}")
            except FileNotFoundError:
                pass

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.destroy() if self._host else self.close()


# ── Demo ───────────────────────────────────────────────────────────────────────────────────────

def main() -> None:
    print(f"[Python Host] Creating Synapse bridge '{CHANNEL_NAME}'...")
    print( "[Python Host] Start cpp_receiver in another terminal, then press Enter.")
    input()

    with SynapseBridge(CHANNEL_NAME, create=True) as bridge:
        print("[Python Host] Bridge ready — sending 10 frames.\n")

        for i in range(10):
            msg = f"frame_{i:04d}|pos=({i*0.1:.2f},{i*0.2:.2f},{i*0.3:.2f})".encode()
            bridge.send(msg)
            print(f"  → [{i:2d}] {msg.decode()}")

            # Poll up to 500 ms for the ACK from the C++ side
            deadline = time.monotonic() + 0.5
            ack = None
            while time.monotonic() < deadline:
                ack = bridge.recv()
                if ack is not None:
                    break
                time.sleep(0.001)

            if ack is not None:
                print(f"  ← [{i:2d}] {ack.decode()}")
            else:
                print(f"  ← [{i:2d}] (no reply within 500 ms)")
            print()

        # Graceful shutdown
        bridge.send(b"__EXIT__")
        print("[Python Host] __EXIT__ sent — draining remaining replies...")
        time.sleep(0.1)

        while True:
            tail_msg = bridge.recv()
            if tail_msg is None:
                break
            print(f"  ← Drain: {tail_msg.decode()}")

    print("[Python Host] Done.")


if __name__ == "__main__":
    main()
