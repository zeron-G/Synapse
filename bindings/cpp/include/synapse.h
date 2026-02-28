// synapse.h — Header-only C++ client for Synapse shared memory bridge.
//
// Cross-platform (Linux + Windows), no external dependencies.
//
// Usage:
//   auto bridge = synapse::host("my_channel");
//   bridge.send(data, len);
//
//   auto bridge = synapse::connect("my_channel");
//   auto msg = bridge.recv();

#pragma once

#include <cstdint>
#include <cstring>
#include <string>
#include <vector>
#include <optional>
#include <stdexcept>
#include <atomic>
#include <cassert>

#ifdef _WIN32
#  ifndef WIN32_LEAN_AND_MEAN
#    define WIN32_LEAN_AND_MEAN
#  endif
#  include <windows.h>
#else
#  include <fcntl.h>
#  include <sys/mman.h>
#  include <unistd.h>
#  include <sys/stat.h>
#endif

namespace synapse {

// ── Constants ──

static constexpr uint64_t MAGIC   = 0x53594E4150534500ULL;  // "SYNAPSE\0"
static constexpr uint32_t VERSION = 1;
static constexpr size_t CACHELINE = 64;
static constexpr size_t CONTROL_SIZE = 256;
static constexpr size_t RING_HEADER_SIZE = 3 * CACHELINE;  // 192 bytes

static constexpr uint64_t DEFAULT_CAPACITY  = 1024;
static constexpr uint64_t DEFAULT_SLOT_SIZE = 256;

// ── Control Block ──

struct ControlBlock {
    uint64_t magic;
    uint32_t version;
    uint32_t flags;
    uint64_t region_size;
    uint64_t creator_pid;
    uint64_t connector_pid;
    uint64_t session_token_lo;
    uint64_t session_token_hi;
    std::atomic<uint64_t> creator_heartbeat;
    std::atomic<uint64_t> connector_heartbeat;
    std::atomic<uint32_t> state;
    uint32_t channel_count;
    // padding to 256 bytes handled by placement
};

// ── Ring Buffer (raw pointer operations, no alignment requirements on the struct itself) ──

class Ring {
    uint8_t* base_;

    std::atomic<uint64_t>& head() const {
        return *reinterpret_cast<std::atomic<uint64_t>*>(base_);
    }
    std::atomic<uint64_t>& tail() const {
        return *reinterpret_cast<std::atomic<uint64_t>*>(base_ + CACHELINE);
    }
    uint64_t capacity() const {
        uint64_t v; std::memcpy(&v, base_ + 2 * CACHELINE, 8); return v;
    }
    uint64_t slot_size() const {
        uint64_t v; std::memcpy(&v, base_ + 2 * CACHELINE + 8, 8); return v;
    }
    uint64_t mask() const {
        uint64_t v; std::memcpy(&v, base_ + 2 * CACHELINE + 16, 8); return v;
    }
    uint8_t* slot_ptr(uint64_t index) const {
        return base_ + RING_HEADER_SIZE + static_cast<size_t>(index * slot_size());
    }

public:
    explicit Ring(uint8_t* base) : base_(base) {}

    bool try_push(const uint8_t* data, size_t len) {
        size_t max_payload = static_cast<size_t>(slot_size()) - 4;
        if (len > max_payload) return false;

        uint64_t h = head().load(std::memory_order_relaxed);
        uint64_t t = tail().load(std::memory_order_acquire);
        if (h - t >= capacity()) return false;

        uint8_t* slot = slot_ptr(h & mask());
        uint32_t l = static_cast<uint32_t>(len);
        std::memcpy(slot, &l, 4);
        std::memcpy(slot + 4, data, len);
        head().store(h + 1, std::memory_order_release);
        return true;
    }

    std::optional<std::vector<uint8_t>> try_pop() {
        uint64_t t = tail().load(std::memory_order_relaxed);
        uint64_t h = head().load(std::memory_order_acquire);
        if (h == t) return std::nullopt;

        uint8_t* slot = slot_ptr(t & mask());
        uint32_t len;
        std::memcpy(&len, slot, 4);
        std::vector<uint8_t> result(slot + 4, slot + 4 + len);
        tail().store(t + 1, std::memory_order_release);
        return result;
    }

    bool empty() const {
        return head().load(std::memory_order_acquire) == tail().load(std::memory_order_acquire);
    }
};

// ── Shared Memory Region ──

class SharedRegion {
    uint8_t* ptr_ = nullptr;
    size_t size_ = 0;
    bool is_creator_ = false;
    std::string name_;
#ifdef _WIN32
    HANDLE handle_ = nullptr;
#endif

public:
    SharedRegion() = default;
    SharedRegion(const SharedRegion&) = delete;
    SharedRegion& operator=(const SharedRegion&) = delete;

    SharedRegion(SharedRegion&& o) noexcept
        : ptr_(o.ptr_), size_(o.size_), is_creator_(o.is_creator_), name_(std::move(o.name_))
#ifdef _WIN32
        , handle_(o.handle_)
#endif
    {
        o.ptr_ = nullptr;
#ifdef _WIN32
        o.handle_ = nullptr;
#endif
    }

    SharedRegion& operator=(SharedRegion&& o) noexcept {
        if (this != &o) { cleanup(); std::swap(ptr_, o.ptr_); std::swap(size_, o.size_);
            std::swap(is_creator_, o.is_creator_); std::swap(name_, o.name_);
#ifdef _WIN32
            std::swap(handle_, o.handle_);
#endif
        }
        return *this;
    }

    ~SharedRegion() { cleanup(); }

    static SharedRegion create(const std::string& name, size_t size) {
        SharedRegion r;
        r.size_ = size;
        r.name_ = name;
        r.is_creator_ = true;
#ifdef _WIN32
        std::string map_name = "Local\\synapse_" + name;
        r.handle_ = CreateFileMappingA(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE,
            static_cast<DWORD>(size >> 32), static_cast<DWORD>(size), map_name.c_str());
        if (!r.handle_) throw std::runtime_error("CreateFileMappingA failed");
        r.ptr_ = static_cast<uint8_t*>(MapViewOfFile(r.handle_, FILE_MAP_ALL_ACCESS, 0, 0, size));
        if (!r.ptr_) { CloseHandle(r.handle_); throw std::runtime_error("MapViewOfFile failed"); }
#else
        std::string path = "/" + name;
        int fd = shm_open(path.c_str(), O_CREAT | O_RDWR | O_EXCL, 0660);
        if (fd < 0) throw std::runtime_error("shm_open create failed");
        if (ftruncate(fd, static_cast<off_t>(size)) != 0) { close(fd); shm_unlink(path.c_str()); throw std::runtime_error("ftruncate failed"); }
        r.ptr_ = static_cast<uint8_t*>(mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0));
        close(fd);
        if (r.ptr_ == MAP_FAILED) { r.ptr_ = nullptr; shm_unlink(path.c_str()); throw std::runtime_error("mmap failed"); }
#endif
        std::memset(r.ptr_, 0, size);
        return r;
    }

    static SharedRegion open(const std::string& name, size_t size) {
        SharedRegion r;
        r.size_ = size;
        r.name_ = name;
        r.is_creator_ = false;
#ifdef _WIN32
        std::string map_name = "Local\\synapse_" + name;
        r.handle_ = OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, map_name.c_str());
        if (!r.handle_) throw std::runtime_error("OpenFileMappingA failed");
        r.ptr_ = static_cast<uint8_t*>(MapViewOfFile(r.handle_, FILE_MAP_ALL_ACCESS, 0, 0, size));
        if (!r.ptr_) { CloseHandle(r.handle_); throw std::runtime_error("MapViewOfFile failed"); }
#else
        std::string path = "/" + name;
        int fd = shm_open(path.c_str(), O_RDWR, 0);
        if (fd < 0) throw std::runtime_error("shm_open open failed");
        r.ptr_ = static_cast<uint8_t*>(mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0));
        close(fd);
        if (r.ptr_ == MAP_FAILED) { r.ptr_ = nullptr; throw std::runtime_error("mmap failed"); }
#endif
        return r;
    }

    uint8_t* ptr() const { return ptr_; }
    size_t size() const { return size_; }

private:
    void cleanup() {
        if (!ptr_) return;
#ifdef _WIN32
        UnmapViewOfFile(ptr_);
        if (handle_) CloseHandle(handle_);
#else
        munmap(ptr_, size_);
        if (is_creator_) {
            std::string path = "/" + name_;
            shm_unlink(path.c_str());
        }
#endif
        ptr_ = nullptr;
    }
};

// ── Bridge ──

class Bridge {
    SharedRegion region_;
    bool is_host_;
    size_t ring_ab_offset_;
    size_t ring_ba_offset_;

    static size_t ring_region_size(uint64_t capacity, uint64_t slot_size) {
        return RING_HEADER_SIZE + static_cast<size_t>(capacity) * static_cast<size_t>(slot_size);
    }

    static size_t total_region_size(uint64_t capacity, uint64_t slot_size) {
        return CONTROL_SIZE + ring_region_size(capacity, slot_size) * 2;
    }

    Ring ring_ab() const { return Ring(region_.ptr() + ring_ab_offset_); }
    Ring ring_ba() const { return Ring(region_.ptr() + ring_ba_offset_); }

    void init_ring(size_t offset, uint64_t capacity, uint64_t slot_size) {
        uint8_t* p = region_.ptr() + offset;
        // head = 0 (already zeroed)
        // tail = 0 (already zeroed)
        uint64_t val;
        val = capacity;  std::memcpy(p + 2 * CACHELINE, &val, 8);
        val = slot_size;  std::memcpy(p + 2 * CACHELINE + 8, &val, 8);
        val = capacity - 1; std::memcpy(p + 2 * CACHELINE + 16, &val, 8);
    }

public:
    Bridge() = default;
    Bridge(Bridge&&) = default;
    Bridge& operator=(Bridge&&) = default;

    /// Send data. Host writes to ring_ab, connector writes to ring_ba.
    bool send(const uint8_t* data, size_t len) {
        if (is_host_) return ring_ab().try_push(data, len);
        else return ring_ba().try_push(data, len);
    }

    bool send(const std::string& s) { return send(reinterpret_cast<const uint8_t*>(s.data()), s.size()); }

    /// Receive data. Host reads from ring_ba, connector reads from ring_ab.
    std::optional<std::vector<uint8_t>> recv() {
        if (is_host_) return ring_ba().try_pop();
        else return ring_ab().try_pop();
    }

    /// Receive as string (convenience).
    std::optional<std::string> recv_string() {
        auto r = recv();
        if (!r) return std::nullopt;
        return std::string(r->begin(), r->end());
    }

    bool is_ready() const {
        auto* cb = reinterpret_cast<const ControlBlock*>(region_.ptr());
        return cb->state.load(std::memory_order_acquire) == 1;
    }

    // ── Factory functions ──

    static Bridge host(const std::string& name,
                       uint64_t capacity = DEFAULT_CAPACITY,
                       uint64_t slot_size = DEFAULT_SLOT_SIZE)
    {
        assert((capacity & (capacity - 1)) == 0 && "capacity must be power of 2");
        Bridge b;
        b.is_host_ = true;
        size_t total = total_region_size(capacity, slot_size);
        b.region_ = SharedRegion::create(name, total);
        b.ring_ab_offset_ = CONTROL_SIZE;
        b.ring_ba_offset_ = CONTROL_SIZE + ring_region_size(capacity, slot_size);

        // Init control block
        auto* cb = reinterpret_cast<ControlBlock*>(b.region_.ptr());
        cb->magic = MAGIC;
        cb->version = VERSION;
        cb->flags = 0;
        cb->region_size = total;
#ifdef _WIN32
        cb->creator_pid = static_cast<uint64_t>(GetCurrentProcessId());
#else
        cb->creator_pid = static_cast<uint64_t>(getpid());
#endif
        cb->connector_pid = 0;
        cb->channel_count = 1;
        cb->state.store(1, std::memory_order_release);  // Ready

        b.init_ring(b.ring_ab_offset_, capacity, slot_size);
        b.init_ring(b.ring_ba_offset_, capacity, slot_size);
        return b;
    }

    static Bridge connect(const std::string& name,
                          uint64_t capacity = DEFAULT_CAPACITY,
                          uint64_t slot_size = DEFAULT_SLOT_SIZE)
    {
        Bridge b;
        b.is_host_ = false;
        size_t total = total_region_size(capacity, slot_size);
        b.region_ = SharedRegion::open(name, total);
        b.ring_ab_offset_ = CONTROL_SIZE;
        b.ring_ba_offset_ = CONTROL_SIZE + ring_region_size(capacity, slot_size);

        auto* cb = reinterpret_cast<const ControlBlock*>(b.region_.ptr());
        if (cb->magic != MAGIC)
            throw std::runtime_error("Bad magic number — not a Synapse region");
        if (cb->version != VERSION)
            throw std::runtime_error("Version mismatch");

        return b;
    }
};

// ── Convenience free functions ──

inline Bridge host(const std::string& name,
                   uint64_t capacity = DEFAULT_CAPACITY,
                   uint64_t slot_size = DEFAULT_SLOT_SIZE) {
    return Bridge::host(name, capacity, slot_size);
}

inline Bridge connect(const std::string& name,
                      uint64_t capacity = DEFAULT_CAPACITY,
                      uint64_t slot_size = DEFAULT_SLOT_SIZE) {
    return Bridge::connect(name, capacity, slot_size);
}

}  // namespace synapse
