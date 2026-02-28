// Synapse Example: C++ receiver (connector side).
//
// Connects to the Synapse bridge created by python_sender.py,
// receives 10 frames, sends "ACK:<frame>" replies, then exits on "__EXIT__".
//
// Build (from project root):
//   Linux:   g++ -std=c++17 -O2 -Ibindings/cpp/include -lrt -o examples/cpp_receiver examples/cpp_receiver.cpp
//   Windows: g++ -std=c++17 -O2 -Ibindings/cpp/include -o examples/cpp_receiver.exe examples/cpp_receiver.cpp
//
// Or from the examples/ directory:
//   Linux:   g++ -std=c++17 -O2 -I../bindings/cpp/include -lrt -o cpp_receiver cpp_receiver.cpp
//   Windows: g++ -std=c++17 -O2 -I../bindings/cpp/include -o cpp_receiver.exe cpp_receiver.cpp
//
// Run python_sender.py first, then start this binary in a second terminal.

#include "synapse.h"
#include <iostream>
#include <thread>
#include <chrono>

// Must match CHANNEL_NAME in python_sender.py.
// Rust/C++ bridge opens "Local\synapse_<name>" on Windows and /dev/shm/<name> on Linux.
static constexpr const char* CHANNEL = "demo";

int main() {
    std::cout << "[C++ Connector] Waiting for bridge '" << CHANNEL << "'..." << std::endl;

    synapse::Bridge bridge;

    // Retry until the Python host has created and initialised the bridge.
    while (true) {
        try {
            bridge = synapse::connect(CHANNEL);
            break;
        } catch (const std::exception& e) {
            std::this_thread::sleep_for(std::chrono::milliseconds(50));
        }
    }

    std::cout << "[C++ Connector] Connected!" << std::endl;

    while (true) {
        auto msg = bridge.recv_string();
        if (!msg) {
            std::this_thread::yield();
            continue;
        }

        if (*msg == "__EXIT__") {
            std::cout << "\n[C++ Connector] Exit signal received." << std::endl;
            break;
        }

        std::cout << "  ← Recv: " << *msg << std::endl;

        // Echo back an ACK
        std::string ack = "ACK:" + *msg;
        bridge.send(ack);
        std::cout << "  → Sent: " << ack << std::endl;
    }

    std::cout << "[C++ Connector] Done." << std::endl;
    return 0;
}
