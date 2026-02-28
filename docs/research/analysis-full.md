# PolyBridge 技术分析报告

> 跨语言运行时桥接库 — 共享内存 + 事件总线实现 Python ↔ C++/C#/Rust/Go 帧级实时双向通信

---

## 1. 技术可行性分析

### 1.1 共享内存跨平台实现差异

| 特性 | Linux | macOS | Windows |
|------|-------|-------|---------|
| POSIX shm | `shm_open` + `mmap` | `shm_open` + `mmap` | ❌ 不支持 |
| System V shm | `shmget`/`shmat` | 支持但已弃用 | ❌ |
| Windows Named SM | ❌ | ❌ | `CreateFileMapping` + `MapViewOfFile` |
| Memory-mapped file | `mmap` on tmpfs/file | `mmap` on file | `CreateFileMapping` on file |
| 匿名 mmap | `MAP_ANONYMOUS` | `MAP_ANON` | `SEC_COMMIT` (pagefile-backed) |
| 大页支持 | `MAP_HUGETLB` | 不支持 | `SEC_LARGE_PAGES` |
| 最大尺寸限制 | `/proc/sys/kernel/shmmax` | `kern.sysv.shmmax` | 物理内存+pagefile |

**推荐方案：双层抽象**

```
平台无关层:  PolyBridge SharedRegion API
                    │
平台适配层:  ┌──────┼──────┐
             Linux  macOS  Windows
             shm_open      CreateFileMapping
             + mmap        + MapViewOfFile
```

实际上最可靠的跨平台方式是 **基于文件的 mmap**（Linux/macOS 用 `mmap`，Windows 用 `CreateFileMapping` 打开同一文件）。tmpfs/ramfs 上的文件可避免磁盘 IO。Windows 上可使用 pagefile-backed named mapping 达到同样效果。

**关键注意事项：**
- macOS 的 `shm_open` 限制名称长度为 30 字符（含 `/` 前缀）
- Windows named mapping 的命名空间在 Session 0 和用户 Session 之间隔离（需 `Global\` 前缀跨 session）
- Linux `shm_open` 创建的对象位于 `/dev/shm`，默认大小为物理内存的 50%
- 所有平台均需处理进程崩溃后的孤儿共享内存清理

### 1.2 环形缓冲区设计：SPSC vs MPMC

**结论：MVP 使用 Lock-free SPSC，后续可扩展为 bounded MPMC。**

| 维度 | SPSC | MPMC |
|------|------|------|
| 延迟 | ~10-30ns per op | ~50-200ns per op |
| 实现复杂度 | 低（两个原子指针） | 高（需 CAS 循环或分段） |
| 缓存友好性 | 极好（head/tail 分 cacheline） | 差（多写者竞争） |
| 适用场景 | 1:1 通道 | 多生产者广播 |
| 正确性验证 | 简单 | 需形式化验证（TLA+/Loom） |

**SPSC Ring Buffer 核心设计：**

```
Cache Line 0 (64B):  [head: u64] [padding: 56B]
Cache Line 1 (64B):  [tail: u64] [padding: 56B]
Cache Line 2+:       [slot_0] [slot_1] ... [slot_N-1]

写入: while (head - tail >= capacity) spin; buf[head % cap] = data; head.store(h+1, Release)
读取: while (head == tail) spin; data = buf[tail % cap]; tail.store(t+1, Release)
```

容量必须是 2 的幂以用位运算替代取模。每个 slot 应对齐到 cacheline 避免 false sharing。

**对于 PolyBridge 的典型场景（Python ↔ 一个 C++ 进程），SPSC 足够。** 多语言端点间使用多条 SPSC 通道（每对方向一条），比单个 MPMC 更快更简单。

### 1.3 Python GIL 的影响及解决方案

**影响分析：**

GIL 的核心问题：Python 线程无法并行执行 CPU-bound 的 Python 字节码。但对 PolyBridge 的影响比想象中小：

1. **共享内存读写本身不受 GIL 限制** — `mmap` 操作通过 C 扩展可在释放 GIL 后执行
2. **序列化/反序列化受 GIL 限制** — 将 Python 对象转为字节需要 GIL
3. **事件轮询可在 GIL 外执行** — 原子变量检查不需要 GIL

**解决方案矩阵：**

| 方案 | 延迟影响 | 复杂度 | 推荐度 |
|------|----------|--------|--------|
| C 扩展中 `Py_BEGIN_ALLOW_THREADS` | 最小 | 低 | ⭐⭐⭐⭐⭐ |
| `multiprocessing.shared_memory` | 中等 | 低 | ⭐⭐⭐⭐ |
| `ctypes` 直接操作 mmap 指针 | 最小 | 中 | ⭐⭐⭐⭐ |
| 子进程 + pickle IPC | 高 | 高 | ⭐⭐ |
| Python 3.13+ free-threaded | 最小 | 低 | ⭐⭐⭐（未来） |
| `cffi` + nogil 回调 | 低 | 中 | ⭐⭐⭐⭐ |

**推荐：核心桥接层用 Rust 编写 Python C 扩展（PyO3），在进入共享内存操作前释放 GIL：**

```rust
#[pyfunction]
fn send_frame(py: Python, channel: &PyChannel, data: &[u8]) -> PyResult<()> {
    // 释放 GIL，在原生代码中操作共享内存
    py.allow_threads(|| {
        channel.inner.write(data)
    }).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}
```

### 1.4 类型系统自动映射与零拷贝

**核心挑战：** Python 对象是引用计数的堆对象，C struct 是固定布局的值类型。真正的"零拷贝"意味着两端直接读写同一块共享内存中的 C 布局数据。

**方案：Schema-driven 固定布局 + Python memoryview**

```
Schema 定义 (IDL):
  struct GameState {
      position: Vec3f,    // offset 0,  size 12
      velocity: Vec3f,    // offset 12, size 12
      health: f32,        // offset 24, size 4
      frame_id: u64,      // offset 28, size 8  (注意对齐填充)
  }

共享内存布局:
  [0x00] position.x: f32
  [0x04] position.y: f32
  [0x08] position.z: f32
  [0x0C] velocity.x: f32
  [0x10] velocity.y: f32
  [0x14] velocity.z: f32
  [0x18] health: f32
  [0x1C] padding: 4 bytes
  [0x20] frame_id: u64
```

**Python 端零拷贝访问（通过 struct/ctypes/numpy）：**

```python
import ctypes, mmap

class Vec3f(ctypes.Structure):
    _fields_ = [("x", ctypes.c_float), ("y", ctypes.c_float), ("z", ctypes.c_float)]

class GameState(ctypes.Structure):
    _fields_ = [
        ("position", Vec3f),
        ("velocity", Vec3f),
        ("health", ctypes.c_float),
        ("_pad", ctypes.c_uint32),
        ("frame_id", ctypes.c_uint64),
    ]

# 直接映射到共享内存，无需拷贝
buf = mmap.mmap(fd, ctypes.sizeof(GameState))
state = GameState.from_buffer(buf)  # 零拷贝！
print(state.position.x)  # 直接读取共享内存
state.health = 95.0       # 直接写入共享内存
```

**对于变长数据（字符串、数组）：** 使用 FlatBuffers/Cap'n Proto 风格的 offset 表 + 数据区，或在固定 header 后跟长度前缀的变长区域。

---

## 2. 架构设计

### 2.1 整体架构

```
┌─────────────────────────────────────────────────────────────────┐
│                      PolyBridge Runtime                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌──────────┐    Shared Memory Region     ┌──────────────────┐  │
│  │  Python   │   ┌───────────────────┐    │  C++ / Rust /    │  │
│  │  Process  │   │  Control Block    │    │  C# / Go Process │  │
│  │          │   │  ┌─────────────┐  │    │                  │  │
│  │ ┌──────┐ │   │  │ Magic/Ver   │  │    │  ┌────────────┐  │  │
│  │ │PyO3  │◄├───┤  │ Lock bits   │  ├───►├──┤ Native FFI │  │  │
│  │ │Bridge│ │   │  │ Heartbeat   │  │    │  │ Bridge     │  │  │
│  │ └──────┘ │   │  └─────────────┘  │    │  └────────────┘  │  │
│  │          │   │                   │    │                  │  │
│  │          │   │  ┌─────────────┐  │    │                  │  │
│  │  send()──┼──►│  │ Ring A→B    │──┼───►│──► recv()       │  │
│  │          │   │  │ (SPSC)      │  │    │                  │  │
│  │          │   │  └─────────────┘  │    │                  │  │
│  │          │   │                   │    │                  │  │
│  │  recv()◄─┼───│  │ Ring B→A    │◄─┼────│──◄ send()       │  │
│  │          │   │  │ (SPSC)      │  │    │                  │  │
│  │          │   │  └─────────────┘  │    │                  │  │
│  │          │   │                   │    │                  │  │
│  │          │   │  ┌─────────────┐  │    │                  │  │
│  │  read()◄─┼──►│  │ Data Slots  │◄─┼───►│──► read/write() │  │
│  │          │   │  │ (Structs)   │  │    │                  │  │
│  │          │   │  └─────────────┘  │    │                  │  │
│  └──────────┘   └───────────────────┘    └──────────────────┘  │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                    Event Bus (futex/eventfd/semaphore)       ││
│  │  Python ──notify()──► eventfd/futex ──wake()──► Native      ││
│  │  Python ◄──wake()──── eventfd/futex ◄──notify()── Native    ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 进程模型

```
                    ┌─────────────────┐
                    │   Orchestrator   │
                    │  (可选, 启动器)   │
                    └────────┬────────┘
                             │ spawn & configure
                   ┌─────────┼─────────┐
                   ▼                   ▼
            ┌─────────────┐    ┌─────────────────┐
            │ Process A   │    │ Process B        │
            │ (Python)    │    │ (C++/Rust/Go/C#) │
            │             │    │                  │
            │ polybridge  │    │ polybridge       │
            │ .connect()  │    │ ::connect()      │
            └─────────────┘    └─────────────────┘
                   │                   │
                   └─────┬─────────────┘
                         ▼
                 Shared Memory Region
                 (OS kernel managed)
```

**连接协议：**
1. 进程 A 调用 `create("channel_name", config)` → 创建共享内存区域 + 写入 control block
2. 进程 B 调用 `connect("channel_name")` → 打开已有区域 → 验证 magic/version → 握手
3. Control block 中的 heartbeat 计数器用于检测对端存活
4. 任一进程退出时，另一端通过 heartbeat 超时检测到并清理

### 2.3 共享内存布局

```
Offset    Size      Field               Description
──────────────────────────────────────────────────────────────────
0x0000    8         magic               0x504F4C5942524447 ("POLYBRDG")
0x0008    4         version             协议版本号
0x000C    4         flags               特性标志位
0x0010    8         region_size         总区域大小
0x0018    8         creator_pid         创建者 PID
0x0020    8         connector_pid       连接者 PID
0x0028    8         creator_heartbeat   创建者心跳（单调递增）
0x0030    8         connector_heartbeat 连接者心跳
0x0038    4         state               连接状态 (INIT/READY/CLOSING/DEAD)
0x003C    4         reserved            保留对齐

── Control Block 结束 (0x40 = 64 bytes, 1 cacheline) ──

0x0040    N         ring_a_to_b         Ring Buffer: A→B 方向
  0x0040  8           head              写入位置（对齐到 cacheline）
  0x0080  8           tail              读取位置（对齐到 cacheline）
  0x00C0  4           capacity          槽位数量（2的幂）
  0x00C4  4           slot_size         每槽大小
  0x00C8  8           mask              capacity - 1
  0x0100  ...         slots[0..N]       数据槽（cacheline 对齐）

0x????    N         ring_b_to_a         Ring Buffer: B→A 方向（同上结构）

0x????    M         data_region         共享数据区（可选，用于大型/持久共享状态）
  - Schema-defined structs
  - 变长数据 heap

0x????    P         event_region        事件通知区
  - futex word (Linux)
  - eventfd / pipe fd (跨平台)
```

### 2.4 事件系统设计

纯轮询（spin-wait）浪费 CPU，纯阻塞（blocking wait）延迟高。PolyBridge 使用 **自适应等待策略**：

```
spin_wait(iterations: 100)     // Phase 1: 自旋 ~100ns
  → yield/pause instructions
busy_poll(iterations: 1000)    // Phase 2: 忙等 ~10μs
  → std::thread::yield_now()
futex_wait(timeout: 1ms)       // Phase 3: 内核等待 ~1ms
  → futex(FUTEX_WAIT) / WaitOnAddress / kevent
```

**跨平台事件原语：**

| 平台 | 低延迟通知 | 阻塞等待 |
|------|-----------|---------|
| Linux | `futex(FUTEX_WAKE)` | `futex(FUTEX_WAIT)` |
| macOS | `os_unfair_lock` / `__ulock_wake` | `__ulock_wait` |
| Windows | `WakeByAddressSingle` | `WaitOnAddress` |

所有三个平台都支持在共享内存中的原子变量上做 wait/wake，这是最高效的 IPC 通知方式。

### 2.5 生命周期管理

```
State Machine:

  create() ──► INITIALIZING ──► READY ◄── connect()
                                  │
                        ┌─────────┼─────────┐
                        ▼                   ▼
                   DRAINING            PEER_LOST
                   (优雅关闭)          (心跳超时)
                        │                   │
                        ▼                   ▼
                      CLOSED              CLOSED
                        │                   │
                        └─────────┬─────────┘
                                  ▼
                            CLEANUP
                        (unlink shm, unmap)
```

**崩溃恢复：**
- 每个进程以 1ms 间隔递增 heartbeat 计数器
- 对端 100ms 无心跳更新 → 判定为 PEER_LOST
- PEER_LOST 后可选：自动重连 / 通知应用层 / 清理退出
- 使用 robust mutex 或 atomic state 避免死锁（不使用跨进程 mutex）

---

## 3. 竞品对比

### 3.1 vs 主流 IPC/绑定方案

| 特性 | PolyBridge | pybind11 | gRPC | ZeroMQ | Cap'n Proto | FlatBuffers |
|------|-----------|----------|------|--------|-------------|-------------|
| **通信模型** | 共享内存 | 同进程 | TCP/HTTP2 | TCP/IPC/inproc | TCP/shared | 序列化格式 |
| **延迟** | ~100ns-1μs | ~10ns(函数调用) | ~100μs-1ms | ~10-50μs | ~1-10μs | N/A(仅序列化) |
| **跨进程** | ✅ | ❌(同进程) | ✅ | ✅ | ✅ | N/A |
| **跨语言** | ✅ 多语言 | C++↔Python | ✅ 多语言 | ✅ 多语言 | ✅ 多语言 | ✅ 多语言 |
| **零拷贝** | ✅ 真·零拷贝 | ✅(同进程) | ❌ | ❌(至少1次拷贝) | ✅(mmap时) | ✅(读取时) |
| **崩溃隔离** | ✅(独立进程) | ❌(崩溃=宿主死) | ✅ | ✅ | ✅ | N/A |
| **帧同步适用** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐(无跨进程) | ⭐⭐(延迟高) | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐ |
| **学习曲线** | 中 | 高 | 中 | 低 | 中 | 低 |

**关键差异化：**
- **vs pybind11：** pybind11 是同进程嵌入，Python 崩溃=C++ 宿主崩溃，且受 GIL 约束。PolyBridge 进程隔离，更安全。
- **vs gRPC：** gRPC 延迟太高（序列化+TCP 至少 100μs），不适合帧级通信（16ms/帧@60FPS）。
- **vs ZeroMQ：** ZeroMQ 的 IPC 模式用 Unix socket，仍有内核态切换开销。PolyBridge 的共享内存完全在用户态。
- **vs Cap'n Proto：** 最接近的竞品。Cap'n Proto 的 RPC 层仍走 socket；PolyBridge 直接共享内存 + 原子操作。

### 3.2 vs 游戏引擎脚本方案

| 方案 | 嵌入模式 | 延迟 | 崩溃隔离 | 生态 |
|------|---------|------|---------|------|
| Lua (LuaJIT) | 同进程嵌入 | ~50ns | ❌ | 小（需自建绑定） |
| GDScript (Godot) | 同进程 | ~100ns | ❌ | Godot 专用 |
| UnrealPython | 同进程插件 | ~1μs | ❌ | UE 专用 |
| PolyBridge | 跨进程 | ~100ns-1μs | ✅ | 通用 |

PolyBridge 的核心优势：**崩溃隔离 + 语言自由**。脚本引擎崩溃不影响宿主，且任何语言都能接入。

### 3.3 已有的共享内存桥接项目

| 项目 | 状态 | 说明 |
|------|------|------|
| **Plasma (Apache Arrow)** | 已弃用 | 共享内存对象存储，用于 ML 管线数据共享，但不做帧同步 |
| **Boost.Interprocess** | 活跃 | C++ 库，提供共享内存原语，但不提供跨语言桥接 |
| **shared-memory-rs** | 活跃 | Rust 共享内存库，仅提供原语不提供协议 |
| **ipc-channel (Servo)** | 活跃 | Rust IPC 通道，但用 socket 不用共享内存 |
| **shmem-ipc** | 小众 | Go 共享内存 IPC，未跨语言 |

**结论：没有现成的跨语言共享内存桥接框架覆盖 PolyBridge 的目标场景。** 这是一个真实的空白。

---

## 4. MVP 实现路线图

### 4.1 核心用 Rust 的理由

1. **零开销抽象** — 原子操作、内存布局控制与 C 一样精确，但有安全保证
2. **跨平台编译** — 同一份 Rust 代码通过 `cfg(target_os)` 编译到 Win/Linux/Mac
3. **一次编写，多语言绑定** — Rust 生态有成熟的绑定生成器：
   - Python: PyO3 + maturin
   - C/C++: `cbindgen` 生成 C 头文件
   - C#: P/Invoke 调用 cdylib
   - Go: cgo 调用 cdylib
4. **`unsafe` 显式标记** — 共享内存操作天然 unsafe，Rust 迫使你标记和审计每一处
5. **`no_std` 可选** — 核心环形缓冲区可 `no_std`，嵌入式也能用

### 4.2 Phase 1：最小可行产品

**范围：**
- Linux + Windows 双平台
- Python ↔ C++ 双向通道
- 固定大小消息的 SPSC ring buffer
- 基本的 create/connect/send/recv API
- 心跳 + 崩溃检测

**预期性能目标：**
- 单消息延迟 < 1μs（64 字节消息）
- 吞吐 > 10M msg/s（64 字节）
- 帧同步延迟 < 100μs（包含 Python 端开销）

### 4.3 可运行代码示例

#### Rust 核心库 (`polybridge-core`)

```rust
// polybridge-core/src/lib.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::ptr;

/// 共享内存区域头部（固定在 offset 0）
#[repr(C, align(64))]
pub struct ControlBlock {
    pub magic: u64,                  // 0x504F4C5942524447
    pub version: u32,
    pub flags: u32,
    pub region_size: u64,
    pub creator_pid: u64,
    pub connector_pid: u64,
    pub creator_heartbeat: AtomicU64,
    pub connector_heartbeat: AtomicU64,
    pub state: AtomicU64,            // 0=INIT, 1=READY, 2=CLOSING, 3=DEAD
}

/// Lock-free SPSC 环形缓冲区
#[repr(C)]
pub struct SpscRingBuffer {
    head: CacheAligned<AtomicU64>,   // 写者拥有
    tail: CacheAligned<AtomicU64>,   // 读者拥有
    capacity: u64,                   // 必须是 2 的幂
    mask: u64,                       // capacity - 1
    slot_size: u64,
    // slots 数据紧跟其后
}

#[repr(C, align(64))]
struct CacheAligned<T>(T);

const MAGIC: u64 = 0x504F4C5942524447;

impl SpscRingBuffer {
    #[inline]
    unsafe fn slot_ptr(&self, index: u64) -> *mut u8 {
        let base = (self as *const Self as *const u8)
            .add(std::mem::size_of::<Self>());
        base as *mut u8
    }

    /// 非阻塞写入
    pub fn try_push(&self, data: &[u8]) -> Result<(), &'static str> {
        assert!(data.len() <= self.slot_size as usize);
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);
        if head - tail >= self.capacity {
            return Err("ring buffer full");
        }
        unsafe {
            let slot = self.slot_ptr(0)
                .add(((head & self.mask) * self.slot_size) as usize);
            ptr::write(slot as *mut u32, data.len() as u32);
            ptr::copy_nonoverlapping(data.as_ptr(), slot.add(4), data.len());
        }
        self.head.0.store(head + 1, Ordering::Release);
        Ok(())
    }

    /// 非阻塞读取
    pub fn try_pop(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let tail = self.tail.0.load(Ordering::Relaxed);
        let head = self.head.0.load(Ordering::Acquire);
        if head == tail {
            return Err("ring buffer empty");
        }
        let len;
        unsafe {
            let slot = self.slot_ptr(0)
                .add(((tail & self.mask) * self.slot_size) as usize);
            len = ptr::read(slot as *const u32) as usize;
            assert!(len <= buf.len());
            ptr::copy_nonoverlapping(slot.add(4), buf.as_mut_ptr(), len);
        }
        self.tail.0.store(tail + 1, Ordering::Release);
        Ok(len)
    }
}

// ── 跨平台共享内存 ──

#[cfg(unix)]
mod platform {
    use std::ffi::CString;

    pub struct SharedRegion {
        ptr: *mut u8,
        size: usize,
        name: String,
        is_creator: bool,
    }

    impl SharedRegion {
        pub fn create(name: &str, size: usize) -> std::io::Result<Self> {
            let cname = CString::new(format!("/{name}"))?;
            unsafe {
                let fd = libc::shm_open(
                    cname.as_ptr(),
                    libc::O_CREAT | libc::O_RDWR | libc::O_EXCL, 0o660,
                );
                if fd < 0 { return Err(std::io::Error::last_os_error()); }
                libc::ftruncate(fd, size as libc::off_t);
                let ptr = libc::mmap(
                    std::ptr::null_mut(), size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED, fd, 0,
                );
                libc::close(fd);
                Ok(Self { ptr: ptr as *mut u8, size, name: name.to_string(), is_creator: true })
            }
        }

        pub fn open(name: &str, size: usize) -> std::io::Result<Self> {
            let cname = CString::new(format!("/{name}"))?;
            unsafe {
                let fd = libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0);
                if fd < 0 { return Err(std::io::Error::last_os_error()); }
                let ptr = libc::mmap(
                    std::ptr::null_mut(), size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED, fd, 0,
                );
                libc::close(fd);
                Ok(Self { ptr: ptr as *mut u8, size, name: name.to_string(), is_creator: false })
            }
        }

        pub fn as_ptr(&self) -> *mut u8 { self.ptr }
    }

    impl Drop for SharedRegion {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.size);
                if self.is_creator {
                    let cname = CString::new(format!("/{}", self.name)).unwrap();
                    libc::shm_unlink(cname.as_ptr());
                }
            }
        }
    }
}
```

#### Python 绑定 (PyO3)

```rust
// polybridge-python/src/lib.rs

use pyo3::prelude::*;
use pyo3::types::PyBytes;

#[pyclass]
struct Channel {
    name: String,
    region_ptr: *mut u8,
    region_size: usize,
}

#[pymethods]
impl Channel {
    #[staticmethod]
    fn create(name: &str, slot_size: usize, capacity: usize) -> PyResult<Self> {
        // 调用 platform::SharedRegion::create 并初始化布局
        Ok(Channel {
            name: name.to_string(),
            region_ptr: std::ptr::null_mut(),
            region_size: 0,
        })
    }

    #[staticmethod]
    fn connect(name: &str) -> PyResult<Self> {
        Ok(Channel {
            name: name.to_string(),
            region_ptr: std::ptr::null_mut(),
            region_size: 0,
        })
    }

    /// 发送数据（释放 GIL）
    fn send(&self, py: Python, data: &[u8]) -> PyResult<()> {
        py.allow_threads(|| {
            // ring_buffer.try_push(data)
            Ok(())
        })
    }

    /// 接收数据（释放 GIL）
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let mut buf = vec![0u8; 4096];
        let len = py.allow_threads(|| -> Result<usize, String> {
            // ring_buffer.try_pop(&mut buf)
            Ok(0)
        }).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
        Ok(PyBytes::new(py, &buf[..len]))
    }
}

#[pymodule]
fn polybridge(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Channel>()?;
    Ok(())
}
```

#### Python 使用端（可直接运行的纯 Python 原型）

```python
#!/usr/bin/env python3
"""polybridge_demo_python.py - Python 端示例（纯 Python，无需 Rust 编译）"""

import struct
import time
from multiprocessing import shared_memory

MAGIC = 0x504F4C5942524447
HEADER_SIZE = 64
RING_META_SIZE = 128

class SpscChannel:
    """基于共享内存的 SPSC 通道"""

    def __init__(self, name: str, slot_size: int = 256, capacity: int = 1024,
                 create: bool = False):
        self.slot_size = slot_size
        self.capacity = capacity
        self.mask = capacity - 1
        assert capacity & self.mask == 0, "capacity must be power of 2"

        ring_data_size = slot_size * capacity
        total_size = HEADER_SIZE + 2 * (RING_META_SIZE + ring_data_size)

        if create:
            self.shm = shared_memory.SharedMemory(name=name, create=True, size=total_size)
            struct.pack_into('<QIIQ', self.shm.buf, 0, MAGIC, 1, 0, total_size)
        else:
            self.shm = shared_memory.SharedMemory(name=name, create=False)
            magic = struct.unpack_from('<Q', self.shm.buf, 0)[0]
            assert magic == MAGIC, f"bad magic: {magic:#x}"

        self._ring_ab_offset = HEADER_SIZE
        self._ring_ba_offset = HEADER_SIZE + RING_META_SIZE + ring_data_size

    def _read_u64(self, offset: int) -> int:
        return struct.unpack_from('<Q', self.shm.buf, offset)[0]

    def _write_u64(self, offset: int, value: int):
        struct.pack_into('<Q', self.shm.buf, offset, value)

    def send(self, data: bytes, ring_offset: int = None):
        if ring_offset is None:
            ring_offset = self._ring_ab_offset
        head = self._read_u64(ring_offset)
        tail = self._read_u64(ring_offset + 64)
        if head - tail >= self.capacity:
            raise BufferError("ring buffer full")
        slot_base = ring_offset + RING_META_SIZE
        slot_offset = slot_base + (head & self.mask) * self.slot_size
        struct.pack_into('<I', self.shm.buf, slot_offset, len(data))
        self.shm.buf[slot_offset + 4 : slot_offset + 4 + len(data)] = data
        self._write_u64(ring_offset, head + 1)

    def recv(self, ring_offset: int = None) -> bytes | None:
        if ring_offset is None:
            ring_offset = self._ring_ab_offset
        tail = self._read_u64(ring_offset + 64)
        head = self._read_u64(ring_offset)
        if head == tail:
            return None
        slot_base = ring_offset + RING_META_SIZE
        slot_offset = slot_base + (tail & self.mask) * self.slot_size
        length = struct.unpack_from('<I', self.shm.buf, slot_offset)[0]
        data = bytes(self.shm.buf[slot_offset + 4 : slot_offset + 4 + length])
        self._write_u64(ring_offset + 64, tail + 1)
        return data

    def close(self):
        self.shm.close()

    def destroy(self):
        self.shm.close()
        self.shm.unlink()


def demo_producer():
    ch = SpscChannel("polybridge_demo", slot_size=256, capacity=1024, create=True)
    print("[Python Producer] Channel created")
    for i in range(10):
        msg = f"frame_{i:04d}|pos=({i*0.1:.1f},{i*0.2:.1f},{i*0.3:.1f})".encode()
        ch.send(msg)
        print(f"[Python Producer] Sent: {msg.decode()}")
        time.sleep(0.016)
    ch.send(b"__EXIT__")
    time.sleep(0.1)
    ch.destroy()
    print("[Python Producer] Done")


if __name__ == "__main__":
    demo_producer()
```

#### C++ 使用端（可直接编译运行）

```cpp
// polybridge_demo_cpp.cpp
// 编译: g++ -std=c++20 -O2 -lrt -o demo_cpp polybridge_demo_cpp.cpp

#include <cstdint>
#include <cstring>
#include <iostream>
#include <atomic>
#include <thread>
#include <chrono>
#include <string>

#ifdef _WIN32
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/mman.h>
#include <unistd.h>
#endif

constexpr uint64_t MAGIC = 0x504F4C5942524447ULL;
constexpr size_t HEADER_SIZE = 64;
constexpr size_t RING_META_SIZE = 128;

class SpscChannel {
    uint8_t* base_ = nullptr;
    size_t slot_size_, capacity_, mask_;
    size_t ring_ab_offset_, ring_ba_offset_;
    size_t total_size_ = 0;

    uint64_t load_u64(size_t off) const {
        return reinterpret_cast<std::atomic<uint64_t>*>(base_ + off)
            ->load(std::memory_order_acquire);
    }
    void store_u64(size_t off, uint64_t v) {
        reinterpret_cast<std::atomic<uint64_t>*>(base_ + off)
            ->store(v, std::memory_order_release);
    }

public:
    bool connect(const char* name, size_t slot_size = 256, size_t capacity = 1024) {
        slot_size_ = slot_size;
        capacity_ = capacity;
        mask_ = capacity - 1;
        size_t ring_data = slot_size * capacity;
        total_size_ = HEADER_SIZE + 2 * (RING_META_SIZE + ring_data);
        ring_ab_offset_ = HEADER_SIZE;
        ring_ba_offset_ = HEADER_SIZE + RING_META_SIZE + ring_data;

#ifdef _WIN32
        HANDLE h = OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, name);
        if (!h) return false;
        base_ = (uint8_t*)MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, total_size_);
#else
        std::string path = std::string("/") + name;
        int fd = shm_open(path.c_str(), O_RDWR, 0);
        if (fd < 0) return false;
        base_ = (uint8_t*)mmap(nullptr, total_size_,
                               PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        close(fd);
#endif
        uint64_t magic;
        std::memcpy(&magic, base_, 8);
        if (magic != MAGIC) {
            std::cerr << "Bad magic!" << std::endl;
            return false;
        }
        std::cout << "[C++] Connected to shared memory" << std::endl;
        return true;
    }

    bool recv(uint8_t* buf, size_t bufsz, size_t& out) {
        size_t r = ring_ab_offset_;
        uint64_t tail = load_u64(r + 64);
        uint64_t head = load_u64(r);
        if (head == tail) return false;
        size_t slot = r + RING_META_SIZE + (tail & mask_) * slot_size_;
        uint32_t len;
        std::memcpy(&len, base_ + slot, 4);
        if (len > bufsz) return false;
        std::memcpy(buf, base_ + slot + 4, len);
        out = len;
        store_u64(r + 64, tail + 1);
        return true;
    }

    bool send(const uint8_t* data, size_t len) {
        size_t r = ring_ba_offset_;
        uint64_t head = load_u64(r);
        uint64_t tail = load_u64(r + 64);
        if (head - tail >= capacity_) return false;
        size_t slot = r + RING_META_SIZE + (head & mask_) * slot_size_;
        uint32_t l = (uint32_t)len;
        std::memcpy(base_ + slot, &l, 4);
        std::memcpy(base_ + slot + 4, data, len);
        store_u64(r, head + 1);
        return true;
    }

    ~SpscChannel() {
        if (base_) {
#ifdef _WIN32
            UnmapViewOfFile(base_);
#else
            munmap(base_, total_size_);
#endif
        }
    }
};

int main() {
    SpscChannel ch;
    std::cout << "[C++] Waiting for Python to create channel..." << std::endl;
    while (!ch.connect("polybridge_demo"))
        std::this_thread::sleep_for(std::chrono::milliseconds(100));

    uint8_t buf[256];
    size_t len;
    while (true) {
        if (ch.recv(buf, sizeof(buf), len)) {
            std::string msg((char*)buf, len);
            if (msg == "__EXIT__") {
                std::cout << "[C++] Exit signal received" << std::endl;
                break;
            }
            std::cout << "[C++] Got: " << msg << std::endl;
            std::string ack = "ACK:" + msg;
            ch.send((const uint8_t*)ack.data(), ack.size());
        } else {
            std::this_thread::yield();
        }
    }
    return 0;
}
```

#### 运行方式

```bash
# 终端 1: 编译并启动 C++ 端（先启动，等待连接）
g++ -std=c++20 -O2 -lrt -o demo_cpp polybridge_demo_cpp.cpp
./demo_cpp

# 终端 2: 运行 Python 端（创建共享内存并发送数据）
python3 polybridge_demo_python.py

# 预期输出 (C++ 端):
# [C++] Waiting for Python to create channel...
# [C++] Connected to shared memory
# [C++] Got: frame_0000|pos=(0.0,0.0,0.0)
# [C++] Got: frame_0001|pos=(0.1,0.2,0.3)
# ...
# [C++] Exit signal received
```

### 4.4 开发周期估算

| Phase | 内容 | 周期 | 人力 |
|-------|------|------|------|
| **Phase 1: Core** | SPSC ring + 跨平台 shm + Python/C++ 绑定 | 4-6 周 | 1 人 |
| **Phase 2: Robustness** | 心跳、崩溃恢复、自适应等待、benchmarks | 3-4 周 | 1 人 |
| **Phase 3: Multi-lang** | C#/Go 绑定 + MPMC 可选 + IDL codegen | 4-6 周 | 1-2 人 |
| **Phase 4: Polish** | 文档、CI/CD、示例项目（游戏引擎集成 demo） | 2-3 周 | 1 人 |
| **总计** | | **13-19 周** | **1-2 人** |

**里程碑：**
- Week 2: Linux 上 Python↔C++ echo benchmark 跑通
- Week 4: Windows 支持 + PyPI 可安装
- Week 6: Phase 1 完成，发布 v0.1.0
- Week 10: 心跳 + 崩溃恢复 + 性能调优完成
- Week 16: 四语言绑定齐全，发布 v0.5.0

---

## 总结

PolyBridge 填补了一个真实的技术空白：**跨语言、跨进程、亚微秒级延迟的双向通信**。现有方案要么同进程不隔离（pybind11），要么延迟太高（gRPC），要么不跨语言（Boost.Interprocess）。

核心技术路线（共享内存 + SPSC ring buffer + 自适应等待）成熟且经过验证。主要风险点：
1. 跨平台共享内存 API 差异封装（中等风险，有成熟参考）
2. Python GIL 性能优化（低风险，PyO3 方案成熟）
3. 崩溃恢复鲁棒性（中等风险，需充分测试）

**建议：先用纯 Python + C++ 原型验证端到端流程（如上代码），再用 Rust 重写核心。**

---

## Claude Review（批注层）

> 以下为对本设计的独立审查，包括 bug 标记、设计盲点和补充建议。
> 整体评价：**架构选型正确，底层机制成熟，但存在几个实现级 bug 和一个关键设计空白。**

---

### R1. 代码 Bug：两处需要修复

#### Bug 1：Rust `slot_ptr` 忽略了 `index` 参数

```rust
// 当前代码（第 432 行）——index 参数被完全忽略
unsafe fn slot_ptr(&self, index: u64) -> *mut u8 {
    let base = (self as *const Self as *const u8).add(std::mem::size_of::<Self>());
    base as *mut u8  // ← 永远返回 slot[0] 的地址
}

// 修正应为：
unsafe fn slot_ptr(&self, index: u64) -> *mut u8 {
    let base = (self as *const Self as *const u8).add(std::mem::size_of::<Self>());
    base.add((index * self.slot_size) as usize) as *mut u8
}
```

这是一个会导致所有写入覆盖同一个 slot 的严重 bug，所有帧数据都会写到 `slot[0]`。

#### Bug 2：Python Demo 的 `recv` 方向错误

```python
# 当前代码：recv 读的是 ring_ab_offset（Python→C++ 方向，自己发出的消息）
def recv(self, ring_offset: int = None) -> bytes | None:
    if ring_offset is None:
        ring_offset = self._ring_ab_offset  # ← 错了，这是 A→B（发送环）

# 修正：Python 收 C++ 的回复应读 ring_ba_offset（B→A 方向）
    if ring_offset is None:
        ring_offset = self._ring_ba_offset
```

当前 Demo 中 Python 调用 `recv()` 实际上是在读自己刚发出去的消息，而不是读 C++ 端的 ACK 回复。这也是为什么 Demo 只展示了 Producer 端而没有展示完整的双向通信验证。

---

### R2. 关键设计空白：IDL/Schema 层是 Phase 1，不是 Phase 3

文档把 "IDL codegen" 放在 Phase 3，这个优先级判断有问题。**Schema 层应该是整个框架的基础，而不是锦上添花。**

理由：

- 没有 IDL，用户需要手动在 Python/C++/C#/Go 四端分别维护相同的内存布局定义，极其容易出现偏移错位（alignment padding 在不同编译器下行为不一致）
- `#pragma pack` 在 C++ 和 `ctypes.Structure` 在 Python 的对齐规则有细微差异，一旦错位，数据静默损坏，调试噩梦
- 如果 Phase 1 不强制 schema-first，后续很难补救

**建议的 Schema 优先级调整：**

```
Phase 1 核心：
  ├── 跨平台 SharedRegion API（原有）
  ├── SPSC ring buffer（原有）
  ├── .bridge IDL 文件格式（新增，提前）
  │     struct GameState {
  │         position: vec3f,   // 自动处理对齐
  │         health: f32,
  │         frame_id: u64,
  │     }
  └── 代码生成器: .bridge → Python ctypes + C++ struct + Rust repr(C)
```

这样 Phase 1 结束后，用户就有一个端到端类型安全的通信系统，而不是裸字节通道。

---

### R3. 拓扑盲点：1:N 多端点问题

文档的架构隐含假设是 **1:1 点对点**（一个 Python ↔ 一个 C++ 进程）。但实际的 AI Agent 场景通常是：

```
         Python AI Brain（推理进程）
              ↑↓
    ┌─────────┼─────────┐
    ↓         ↓         ↓
  C++ NPC_1  C++ NPC_2  C++ NPC_3   （同一个游戏进程内的多个实体）
```

或者反过来：

```
  Python Agent_1（技能 AI）
  Python Agent_2（对话 AI）      ← 多个 Python 进程
  Python Agent_3（决策 AI）
         ↑↓
    C++ Game Engine
```

**当前设计对此没有答案。** 有两个选项：

| 方案 | 延迟 | 复杂度 | 适合场景 |
|------|------|--------|---------|
| **多条 SPSC 通道（N×2 个环）** | 最低 | 低 | N 固定，静态配置 |
| **中心 Hub 进程（PolyBridge Broker）** | +1-2μs | 高 | N 动态，运行时增减端点 |
| **MPMC 单环（多生产者多消费者）** | 中 | 中 | 广播模型 |

对于游戏 AI 场景，推荐 **多 SPSC 通道** 方案（每个 NPC 一个 channel id），在控制块里用一个 channel registry 记录所有活跃通道：

```
Control Block 扩展：
  channel_count: u32
  channels[]: { id: u64, ab_offset: u64, ba_offset: u64, state: u32 }
```

---

### R4. AI Agent 专项考量（原文档缺失）

这是用户真实场景，但文档几乎没有针对性设计。AI 推理进程和游戏循环的根本矛盾：

```
游戏帧循环：  固定 16ms/帧，严格同步，不能阻塞
AI 推理：     变长，LLM 可能 200ms-2s，完全异步
```

这意味着：

**必须异步解耦，不能做帧同步等待**

```
Game Engine (C++)                Python AI Process
     │                                │
     │──── send(state_frame) ────────►│
     │                                │  [LLM 推理中，可能耗时 500ms]
     │  [继续跑后续帧，不等待]          │
     │                                │
     │◄─── send(action_decision) ─────│  [推理完成，异步写回]
     │                                │
```

**需要增加的设计：**

1. **Action Mailbox（行为邮箱）**：AI 输出写入共享内存中的固定 slot，游戏每帧检查是否有新决策，有则取用，无则用上一帧决策或默认行为

2. **背压（Backpressure）策略**：当 ring buffer 满时（AI 太慢，Game 塞满了 state）怎么办？三个选项：
   - **Drop oldest**：游戏不停更新，AI 只处理最新帧（推荐，AI 不需要历史帧）
   - **Block sender**：游戏等待（绝对不行，会卡帧）
   - **Ring with overwrite**：单槽"最新状态"语义，不是 FIFO 而是 "最新值" slot

3. **时间戳同步**：AI 决策对应的是哪一帧的状态？需要在 header 中携带 `frame_id`，否则 AI 的决策可能应用到错误的游戏状态上

**推荐为 AI Agent 场景专设一种 "Latest-Value Slot"（非 FIFO）的通道类型：**

```
LatestSlot（对比 SPSC Ring）:
  - 容量 = 1 个 slot
  - 写操作：无论是否被读，直接覆盖（游戏写 AI 输入）
  - 读操作：读取当前值 + version counter（AI 检查是否有新输入）
  - 语义：AI 永远读到 "最新游戏状态"，不积压
```

---

### R5. 安全考量（缺失）

Named shared memory 对同主机所有进程可见，存在两个风险：

1. **命名冲突**：另一个进程恰好使用同名 mapping，会拿到你的数据或使你 connect 到错误区域 → 应在 magic 验证之外加 `session_id`（随机 UUID 写入 control block，连接时比对）

2. **恶意进程接入**：任何本地进程都能 attach 到你的共享内存并读写 → 在 Linux 可用 `shm_open` + 文件权限（`0600`）限制，但跨语言场景要确保各端使用相同 uid

**最小修复：** control block 加一个 `session_token: u128`（创建时随机生成，写入 ENV 或 argv 传给子进程，connect 时验证）

---

### R6. 与 WASM Component Model 的对比（补充竞品）

原文竞品分析漏掉了一个重要方向：

| | PolyBridge | WASM Component Model (WIT) |
|--|------------|---------------------------|
| **延迟** | ~100ns | ~1-10μs（WASM JIT + 接口类型转换） |
| **零拷贝** | ✅ 真零拷贝 | ❌ 需要 canonical ABI 转换 |
| **语言支持** | 需要为每种语言写绑定 | Rust/C++/Python/Go/C# 已有 wit-bindgen |
| **类型安全** | 需要自建 IDL | ✅ WIT 天然类型安全 |
| **运行时隔离** | ✅ OS 进程隔离 | ✅ WASM 沙箱隔离 |
| **工具链成熟度** | 需要自建 | 2024-2025 快速成熟中 |
| **适合场景** | 帧级实时（<1μs 要求） | 函数调用级（>10μs 可接受） |

**结论**：如果延迟要求是 <1μs，PolyBridge 有存在价值；如果是 AI 推理场景（推理本身就是 100ms 量级），WASM Component Model 是更省力的方向，等工具链成熟后可能取代这个 niche。PolyBridge 的真正目标用户是**帧同步游戏逻辑 + Python AI 决策**这个交叉点。

---

### R7. 总体评分

| 维度 | 评分 | 说明 |
|------|------|------|
| 底层机制选型 | ★★★★★ | SPSC + shm + PyO3 组合是最优解 |
| 架构分层清晰度 | ★★★★☆ | 控制块/环缓冲/数据区分离设计好 |
| 代码示例质量 | ★★★☆☆ | 有 2 个实现级 bug，Python 原型缺完整双向验证 |
| AI Agent 场景适配 | ★★☆☆☆ | 几乎没有针对异步推理的设计考量 |
| 多端点拓扑 | ★★☆☆☆ | 隐含 1:1 假设，真实场景通常是 1:N |
| IDL/Schema 优先级 | ★★★☆☆ | 放 Phase 3 过晚，应是基础设施 |
| 安全性 | ★★☆☆☆ | 未讨论命名冲突和访问控制 |

**一句话评价：** 这是一个选型正确的好设计，但它描述的是一个"帧同步双进程"的简单场景。如果目标是通用 AI Agent 框架，需要在异步解耦、1:N 拓扑和 Schema-first 三个方向上深化设计。
