// Synapse Example: C++ receiver (connector side).
//
// This connects to a Synapse bridge created by python_sender.py,
// receives frames, and sends ACK replies.
//
// Build:
//   g++ -std=c++17 -O2 -I../bindings/cpp/include -lrt -o cpp_receiver cpp_receiver.cpp
//
// Run python_sender.py first, then:
//   ./cpp_receiver

#include "synapse.h"
#include <iostream>
#include <thread>
#include <chrono>

int main() {
    std::cout << "[C++ Connector] Waiting for bridge..." << std::endl;

    synapse::Bridge bridge;
    while (true) {
        try {
            bridge = synapse::connect("synapse_demo");
            break;
        } catch (const std::exception&) {
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
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
            std::cout << "[C++ Connector] Exit signal received." << std::endl;
            break;
        }

        std::cout << "  ← Recv: " << *msg << std::endl;

        // Send ACK back
        std::string ack = "ACK:" + *msg;
        bridge.send(ack);
        std::cout << "  → Sent: " << ack << std::endl;
    }

    std::cout << "[C++ Connector] Done." << std::endl;
    return 0;
}
