//! Adaptive wait strategies for ring buffer consumers.
//!
//! Replaces busy-spinning with a configurable progression:
//!   Spin → Yield → Park (futex on Linux, WaitOnAddress on Windows)
//!
//! # Strategies
//! - `Spin` — Pure spin loop (lowest latency, highest CPU usage)
//! - `Yield` — Spin then yield to OS scheduler
//! - `Park` — Immediately park on OS primitive
//! - `Adaptive { spin_count, yield_count }` — Spin N times, yield M times, then park

use std::sync::atomic::AtomicU32;
use std::time::Duration;

/// Wait strategy for ring buffer consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStrategy {
    /// Pure spin loop — lowest latency, highest CPU.
    Spin,
    /// Yield to OS scheduler after exhausting spin budget.
    Yield,
    /// Park immediately using OS futex/WaitOnAddress.
    Park,
    /// Adaptive: spin `spin_count` times, yield `yield_count` times, then park.
    Adaptive { spin_count: u32, yield_count: u32 },
}

impl Default for WaitStrategy {
    fn default() -> Self {
        WaitStrategy::Adaptive {
            spin_count: 100,
            yield_count: 10,
        }
    }
}

/// A waiter that blocks until a condition is met, using the chosen strategy.
pub struct Waiter {
    strategy: WaitStrategy,
}

impl Waiter {
    pub fn new(strategy: WaitStrategy) -> Self {
        Self { strategy }
    }

    /// Wait until `condition` returns `true`, using the configured strategy.
    ///
    /// `wake_addr` is an atomic value that the waker will modify to signal readiness.
    /// `expected` is the value that indicates "not ready" (we wait while addr == expected).
    pub fn wait_until(
        &self,
        wake_addr: &AtomicU32,
        expected: u32,
        condition: impl Fn() -> bool,
        timeout: Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;

        match self.strategy {
            WaitStrategy::Spin => self.wait_spin(&condition, deadline),
            WaitStrategy::Yield => self.wait_yield(&condition, deadline),
            WaitStrategy::Park => self.wait_park(wake_addr, expected, &condition, deadline),
            WaitStrategy::Adaptive {
                spin_count,
                yield_count,
            } => self.wait_adaptive(
                wake_addr,
                expected,
                &condition,
                deadline,
                spin_count,
                yield_count,
            ),
        }
    }

    fn wait_spin(&self, condition: &dyn Fn() -> bool, deadline: std::time::Instant) -> bool {
        loop {
            if condition() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::hint::spin_loop();
        }
    }

    fn wait_yield(&self, condition: &dyn Fn() -> bool, deadline: std::time::Instant) -> bool {
        loop {
            if condition() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
    }

    fn wait_park(
        &self,
        wake_addr: &AtomicU32,
        expected: u32,
        condition: &dyn Fn() -> bool,
        deadline: std::time::Instant,
    ) -> bool {
        loop {
            if condition() {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline - now;
            park_with_timeout(wake_addr, expected, remaining);
        }
    }

    fn wait_adaptive(
        &self,
        wake_addr: &AtomicU32,
        expected: u32,
        condition: &dyn Fn() -> bool,
        deadline: std::time::Instant,
        spin_count: u32,
        yield_count: u32,
    ) -> bool {
        // Phase 1: Spin
        for _ in 0..spin_count {
            if condition() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::hint::spin_loop();
        }

        // Phase 2: Yield
        for _ in 0..yield_count {
            if condition() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }

        // Phase 3: Park
        self.wait_park(wake_addr, expected, condition, deadline)
    }
}

/// Wake one waiter blocked on the given address.
///
/// The caller must update the value at `wake_addr` before calling this,
/// so that the futex/WaitOnAddress sees a changed value.
pub fn wake_one(wake_addr: &AtomicU32) {
    wake_impl(wake_addr);
}

// ── Platform-specific park/wake ──

#[cfg(target_os = "linux")]
fn park_with_timeout(addr: &AtomicU32, expected: u32, timeout: Duration) {
    use std::ptr;

    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_nsec: timeout.subsec_nanos() as libc::c_long,
    };

    // FUTEX_WAIT: sleep if *addr == expected
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr as *const AtomicU32 as *const u32,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            &ts as *const libc::timespec,
            ptr::null::<u32>(),
            0u32,
        );
    }
}

#[cfg(target_os = "linux")]
fn wake_impl(addr: &AtomicU32) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr as *const AtomicU32 as *const u32,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1i32, // wake at most 1 waiter
        );
    }
}

#[cfg(windows)]
fn park_with_timeout(addr: &AtomicU32, expected: u32, timeout: Duration) {
    // WaitOnAddress: sleep if *addr == expected
    let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
    unsafe {
        windows_sys::Win32::System::Threading::WaitOnAddress(
            addr as *const AtomicU32 as *const core::ffi::c_void,
            &expected as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>(),
            millis,
        );
    }
}

#[cfg(windows)]
fn wake_impl(addr: &AtomicU32) {
    unsafe {
        windows_sys::Win32::System::Threading::WakeByAddressSingle(
            addr as *const AtomicU32 as *const core::ffi::c_void,
        );
    }
}

// Fallback for non-Linux Unix (macOS, etc.) — use condvar-like sleep
#[cfg(all(unix, not(target_os = "linux")))]
fn park_with_timeout(_addr: &AtomicU32, _expected: u32, timeout: Duration) {
    // No futex available — fall back to short sleep
    let sleep_time = timeout.min(Duration::from_millis(1));
    std::thread::sleep(sleep_time);
}

#[cfg(all(unix, not(target_os = "linux")))]
fn wake_impl(_addr: &AtomicU32) {
    // No-op on non-Linux Unix — the parker will wake on its own via sleep timeout
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_spin_strategy() {
        let flag = Arc::new(AtomicU32::new(0));
        let flag2 = Arc::clone(&flag);

        let handle = thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            flag2.store(1, Ordering::Release);
        });

        let waiter = Waiter::new(WaitStrategy::Spin);
        let result = waiter.wait_until(
            &flag,
            0,
            || flag.load(Ordering::Acquire) == 1,
            Duration::from_secs(2),
        );

        assert!(result, "spin wait should succeed");
        handle.join().unwrap();
    }

    #[test]
    fn test_yield_strategy() {
        let flag = Arc::new(AtomicU32::new(0));
        let flag2 = Arc::clone(&flag);

        let handle = thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            flag2.store(1, Ordering::Release);
        });

        let waiter = Waiter::new(WaitStrategy::Yield);
        let result = waiter.wait_until(
            &flag,
            0,
            || flag.load(Ordering::Acquire) == 1,
            Duration::from_secs(2),
        );

        assert!(result, "yield wait should succeed");
        handle.join().unwrap();
    }

    #[test]
    fn test_park_strategy() {
        let flag = Arc::new(AtomicU32::new(0));
        let flag2 = Arc::clone(&flag);

        let handle = thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag2.store(1, Ordering::Release);
            wake_one(&flag2);
        });

        let waiter = Waiter::new(WaitStrategy::Park);
        let result = waiter.wait_until(
            &flag,
            0,
            || flag.load(Ordering::Acquire) == 1,
            Duration::from_secs(2),
        );

        assert!(result, "park wait should succeed");
        handle.join().unwrap();
    }

    #[test]
    fn test_adaptive_strategy() {
        let flag = Arc::new(AtomicU32::new(0));
        let flag2 = Arc::clone(&flag);

        let handle = thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag2.store(1, Ordering::Release);
            wake_one(&flag2);
        });

        let waiter = Waiter::new(WaitStrategy::Adaptive {
            spin_count: 50,
            yield_count: 5,
        });
        let result = waiter.wait_until(
            &flag,
            0,
            || flag.load(Ordering::Acquire) == 1,
            Duration::from_secs(2),
        );

        assert!(result, "adaptive wait should succeed");
        handle.join().unwrap();
    }

    #[test]
    fn test_timeout() {
        let flag = AtomicU32::new(0);

        let waiter = Waiter::new(WaitStrategy::Adaptive {
            spin_count: 10,
            yield_count: 5,
        });
        let start = std::time::Instant::now();
        let result = waiter.wait_until(
            &flag,
            0,
            || false, // never satisfied
            Duration::from_millis(100),
        );
        let elapsed = start.elapsed();

        assert!(!result, "should timeout");
        assert!(
            elapsed >= Duration::from_millis(90),
            "should have waited ~100ms, got {:?}",
            elapsed
        );
    }

    #[test]
    fn test_already_satisfied() {
        let flag = AtomicU32::new(1);

        for strategy in [
            WaitStrategy::Spin,
            WaitStrategy::Yield,
            WaitStrategy::Park,
            WaitStrategy::Adaptive {
                spin_count: 100,
                yield_count: 10,
            },
        ] {
            let waiter = Waiter::new(strategy);
            let result = waiter.wait_until(
                &flag,
                0,
                || flag.load(Ordering::Acquire) == 1,
                Duration::from_millis(100),
            );
            assert!(
                result,
                "already satisfied should return immediately for {:?}",
                strategy
            );
        }
    }

    #[test]
    fn test_adaptive_transitions_through_phases() {
        // Verify that adaptive actually goes through spin → yield → park
        // by setting very low spin/yield counts and a condition that takes time
        let flag = Arc::new(AtomicU32::new(0));
        let flag2 = Arc::clone(&flag);

        let handle = thread::spawn(move || {
            // Wait long enough to force transition through all phases
            std::thread::sleep(Duration::from_millis(100));
            flag2.store(1, Ordering::Release);
            wake_one(&flag2);
        });

        let waiter = Waiter::new(WaitStrategy::Adaptive {
            spin_count: 2,  // very few spins
            yield_count: 2, // very few yields
        });

        let result = waiter.wait_until(
            &flag,
            0,
            || flag.load(Ordering::Acquire) == 1,
            Duration::from_secs(5),
        );

        assert!(result, "adaptive wait should succeed after park phase");
        handle.join().unwrap();
    }

    #[test]
    fn test_default_strategy() {
        let strategy = WaitStrategy::default();
        assert_eq!(
            strategy,
            WaitStrategy::Adaptive {
                spin_count: 100,
                yield_count: 10,
            }
        );
    }

    #[test]
    fn test_wake_one() {
        let flag = Arc::new(AtomicU32::new(0));
        let flag2 = Arc::clone(&flag);

        let handle = thread::spawn(move || {
            let waiter = Waiter::new(WaitStrategy::Park);
            waiter.wait_until(
                &flag2,
                0,
                || flag2.load(Ordering::Acquire) != 0,
                Duration::from_secs(5),
            )
        });

        std::thread::sleep(Duration::from_millis(50));
        flag.store(42, Ordering::Release);
        wake_one(&flag);

        let result = handle.join().unwrap();
        assert!(result, "parked thread should have been woken");
    }
}
