#!/usr/bin/env python3
"""Synapse Example: Python sender (host side).

This script creates a Synapse bridge and sends frames to a C++ receiver.
Run this FIRST, then start cpp_receiver.

Usage:
    python3 examples/python_sender.py
"""

import time

# When using the compiled PyO3 module:
# import synapse
# bridge = synapse.host("synapse_demo")
# bridge.send(b"hello")
# response = bridge.recv()

# Standalone pure-Python implementation for demo purposes:
import struct
from multiprocessing import shared_memory

MAGIC = 0x53594E4150534500
CONTROL_SIZE = 256
CACHELINE = 64
RING_HEADER_SIZE = 3 * CACHELINE  # 192 bytes

CAPACITY = 1024
SLOT_SIZE = 256


def ring_region_size(capacity, slot_size):
    return RING_HEADER_SIZE + capacity * slot_size


def total_region_size(capacity, slot_size):
    return CONTROL_SIZE + ring_region_size(capacity, slot_size) * 2


class SynapseBridge:
    def __init__(self, name, create=False):
        self.capacity = CAPACITY
        self.slot_size = SLOT_SIZE
        self.mask = CAPACITY - 1
        self.total_size = total_region_size(CAPACITY, SLOT_SIZE)
        self.ring_ab_off = CONTROL_SIZE
        self.ring_ba_off = CONTROL_SIZE + ring_region_size(CAPACITY, SLOT_SIZE)

        if create:
            self.shm = shared_memory.SharedMemory(name=name, create=True, size=self.total_size)
            buf = self.shm.buf
            # Control block
            struct.pack_into('<Q', buf, 0, MAGIC)          # magic
            struct.pack_into('<I', buf, 8, 1)              # version
            struct.pack_into('<Q', buf, 16, self.total_size)  # region_size
            # Init ring_ab
            self._init_ring(self.ring_ab_off)
            # Init ring_ba
            self._init_ring(self.ring_ba_off)
            # state = Ready (1) at offset for state field
            struct.pack_into('<I', buf, 88, 1)
            self.is_host = True
        else:
            self.shm = shared_memory.SharedMemory(name=name, create=False)
            magic = struct.unpack_from('<Q', self.shm.buf, 0)[0]
            if magic != MAGIC:
                raise RuntimeError(f"Bad magic: {magic:#018x}")
            self.is_host = False

    def _init_ring(self, offset):
        buf = self.shm.buf
        # head = 0 (already zeroed)
        # tail = 0 (already zeroed)
        # capacity at offset + 128
        struct.pack_into('<Q', buf, offset + 2 * CACHELINE, self.capacity)
        struct.pack_into('<Q', buf, offset + 2 * CACHELINE + 8, self.slot_size)
        struct.pack_into('<Q', buf, offset + 2 * CACHELINE + 16, self.mask)

    def _ring_push(self, ring_off, data):
        buf = self.shm.buf
        head = struct.unpack_from('<Q', buf, ring_off)[0]
        tail = struct.unpack_from('<Q', buf, ring_off + CACHELINE)[0]
        if head - tail >= self.capacity:
            raise BufferError("ring full")
        slot = ring_off + RING_HEADER_SIZE + (head & self.mask) * self.slot_size
        struct.pack_into('<I', buf, slot, len(data))
        buf[slot + 4: slot + 4 + len(data)] = data
        struct.pack_into('<Q', buf, ring_off, head + 1)

    def _ring_pop(self, ring_off):
        buf = self.shm.buf
        tail = struct.unpack_from('<Q', buf, ring_off + CACHELINE)[0]
        head = struct.unpack_from('<Q', buf, ring_off)[0]
        if head == tail:
            return None
        slot = ring_off + RING_HEADER_SIZE + (tail & self.mask) * self.slot_size
        length = struct.unpack_from('<I', buf, slot)[0]
        data = bytes(buf[slot + 4: slot + 4 + length])
        struct.pack_into('<Q', buf, ring_off + CACHELINE, tail + 1)
        return data

    def send(self, data: bytes):
        """Host sends via ring_ab, connector sends via ring_ba."""
        if self.is_host:
            self._ring_push(self.ring_ab_off, data)
        else:
            self._ring_push(self.ring_ba_off, data)

    def recv(self):
        """Host reads from ring_ba, connector reads from ring_ab."""
        if self.is_host:
            return self._ring_pop(self.ring_ba_off)
        else:
            return self._ring_pop(self.ring_ab_off)

    def close(self):
        self.shm.close()

    def destroy(self):
        self.shm.close()
        self.shm.unlink()


def main():
    bridge = SynapseBridge("synapse_demo", create=True)
    print("[Python Host] Bridge created, sending frames...")

    for i in range(10):
        msg = f"frame_{i:04d}|pos=({i*0.1:.1f},{i*0.2:.1f},{i*0.3:.1f})".encode()
        bridge.send(msg)
        print(f"  → Sent: {msg.decode()}")
        time.sleep(0.016)  # ~60 FPS

        # Check for responses from C++ side
        reply = bridge.recv()
        if reply:
            print(f"  ← Recv: {reply.decode()}")

    # Send exit signal
    bridge.send(b"__EXIT__")
    print("[Python Host] Exit signal sent, waiting for cleanup...")
    time.sleep(0.5)

    # Read any remaining replies
    while True:
        reply = bridge.recv()
        if reply is None:
            break
        print(f"  ← Recv: {reply.decode()}")

    bridge.destroy()
    print("[Python Host] Done.")


if __name__ == "__main__":
    main()
