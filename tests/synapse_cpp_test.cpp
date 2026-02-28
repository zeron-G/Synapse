// synapse_cpp_test.cpp
//
// Compile-and-run test for the synapse.h header-only C++ library.
//
// Validates compile-time constants and in-process bridge creation /
// bidirectional messaging without requiring a second process.
//
// Build from the project root:
//   Linux:   g++ -std=c++17 -O2 -Ibindings/cpp/include -lrt \
//                -o /tmp/synapse_cpp_test tests/synapse_cpp_test.cpp
//   Windows: g++ -std=c++17 -O2 -Ibindings/cpp/include \
//                -o synapse_cpp_test.exe tests/synapse_cpp_test.cpp
//
// Run: /tmp/synapse_cpp_test   (returns 0 on success, 1 on failure)

#include "synapse.h"

#include <cassert>
#include <cstring>
#include <iostream>
#include <string>

// ── Compile-time constant checks ──────────────────────────────────────────────

static_assert(synapse::MAGIC == 0x53594E4150534500ULL, "MAGIC mismatch");
static_assert(synapse::VERSION == 1u, "VERSION mismatch");
static_assert(synapse::CONTROL_SIZE == 256u, "CONTROL_SIZE mismatch");
static_assert(synapse::RING_HEADER_SIZE == 192u, "RING_HEADER_SIZE mismatch");
static_assert(synapse::DEFAULT_CAPACITY == 1024u, "DEFAULT_CAPACITY mismatch");
static_assert(synapse::DEFAULT_SLOT_SIZE == 256u, "DEFAULT_SLOT_SIZE mismatch");

// ── Test scaffolding ──────────────────────────────────────────────────────────

static int g_passed = 0;
static int g_failed = 0;

#define CHECK(expr, msg)                                         \
    do {                                                         \
        if (!(expr)) {                                           \
            std::cerr << "  FAIL  " << (msg) << "\n";           \
            ++g_failed;                                          \
        } else {                                                 \
            std::cout << "  PASS  " << (msg) << "\n";           \
            ++g_passed;                                          \
        }                                                        \
    } while (0)

// ── Shared memory name used by all tests ─────────────────────────────────────

static const char* TEST_NAME = "syntest_cpp_hdr";

static void cleanup_shm() {
#ifndef _WIN32
    // POSIX shm objects live under /dev/shm on Linux.
    std::string path = std::string("/dev/shm/") + TEST_NAME;
    ::remove(path.c_str()); // ignore ENOENT
#endif
}

// ── Individual tests ──────────────────────────────────────────────────────────

static void test_constants() {
    // static_assert above already verified values at compile time.
    CHECK(synapse::MAGIC == 0x53594E4150534500ULL, "MAGIC value correct");
    CHECK(synapse::VERSION == 1u, "VERSION value correct");
    CHECK(synapse::CONTROL_SIZE == 256u, "CONTROL_SIZE correct");
    CHECK(synapse::DEFAULT_CAPACITY == 1024u, "DEFAULT_CAPACITY correct");
    CHECK(synapse::DEFAULT_SLOT_SIZE == 256u, "DEFAULT_SLOT_SIZE correct");
}

static void test_host_connect_roundtrip() {
    cleanup_shm();

    auto host = synapse::host(TEST_NAME);
    auto conn = synapse::connect(TEST_NAME);

    CHECK(host.is_ready(), "host.is_ready()");
    CHECK(conn.is_ready(), "conn.is_ready()");

    // Host → connector
    {
        const std::string msg = "hello from cpp host";
        bool sent = host.send(msg);
        CHECK(sent, "host.send() returns true");

        auto recv = conn.recv_string();
        CHECK(recv.has_value(), "connector received message");
        CHECK(recv.has_value() && *recv == msg, "connector recv content matches");
    }

    // Connector → host
    {
        const std::string reply = "reply from connector";
        bool sent = conn.send(reply);
        CHECK(sent, "conn.send() returns true");

        auto recv = host.recv_string();
        CHECK(recv.has_value(), "host received reply");
        CHECK(recv.has_value() && *recv == reply, "host recv content matches");
    }
}

static void test_empty_recv() {
    cleanup_shm();

    auto host = synapse::host(TEST_NAME);
    auto conn = synapse::connect(TEST_NAME);

    auto empty_h = host.recv();
    auto empty_c = conn.recv();
    CHECK(!empty_h.has_value(), "host empty recv returns nullopt");
    CHECK(!empty_c.has_value(), "conn empty recv returns nullopt");
}

static void test_multiple_messages() {
    cleanup_shm();

    auto host = synapse::host(TEST_NAME);
    auto conn = synapse::connect(TEST_NAME);

    constexpr int N = 50;
    bool all_sent = true;
    for (int i = 0; i < N; ++i) {
        std::string msg = "item_" + std::to_string(i);
        if (!host.send(msg)) {
            all_sent = false;
            break;
        }
    }
    CHECK(all_sent, "host sent all 50 messages");

    bool all_recvd = true;
    for (int i = 0; i < N; ++i) {
        auto r = conn.recv_string();
        std::string expected = "item_" + std::to_string(i);
        if (!r.has_value() || *r != expected) {
            all_recvd = false;
            break;
        }
    }
    CHECK(all_recvd, "connector received all 50 messages in order");

    // Ring should now be empty.
    CHECK(!conn.recv().has_value(), "no extra messages after drain");
}

static void test_control_block_fields() {
    cleanup_shm();

    auto host = synapse::host(TEST_NAME);

    // Inspect the raw control block to verify field values.
    const uint8_t* ptr = nullptr;
    // Access region pointer via the bridge's internal layout.
    // We verify the ControlBlock struct is the right size (<= 256 bytes).
    CHECK(sizeof(synapse::ControlBlock) <= 256u, "ControlBlock fits in 256 bytes");

    // Verify the constants match the ControlBlock field interpretation.
    CHECK(synapse::RING_HEADER_SIZE == 3 * synapse::CACHELINE,
          "RING_HEADER_SIZE == 3 * CACHELINE");
    (void)ptr;
}

// ── Main ──────────────────────────────────────────────────────────────────────

int main() {
    std::cout << "================================================\n";
    std::cout << "  Synapse C++ Header Tests\n";
    std::cout << "================================================\n";

    test_constants();
    test_host_connect_roundtrip();
    cleanup_shm();
    test_empty_recv();
    cleanup_shm();
    test_multiple_messages();
    cleanup_shm();
    test_control_block_fields();
    cleanup_shm();

    std::cout << "\n================================================\n";
    if (g_failed == 0) {
        std::cout << "  All " << g_passed << " checks passed\n";
    } else {
        std::cout << "  FAILED: " << g_failed << " / "
                  << (g_passed + g_failed) << " checks\n";
    }
    std::cout << "================================================\n";

    return (g_failed == 0) ? 0 : 1;
}
