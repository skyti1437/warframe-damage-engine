//! 原生回放的低开销性能计数器。
//!
//! 前三项始终启用。函数级计时包含子调用，应通过 `enable(true)` 按需启用，因为时钟读取的开销与部分计算相近。
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

pub const TOTAL: usize = 0;
pub const RESOLVE: usize = 1;
pub const VIEW: usize = 2;
pub const CLEAN: usize = 3;
pub const TAKE: usize = 4;
pub const ADD_PROCS: usize = 5;
pub const MODEL: usize = 6;
pub const PACKET: usize = 7;
pub const SIGNAL: usize = 8;
pub const FIRE: usize = 9;
pub const SETUP: usize = 10;
pub const RESULT: usize = 11;
pub const PRESENCE: usize = 12;
pub const HEAT: usize = 13;
pub const POOLED: usize = 14;
pub const NOSTATUS: usize = 15;
pub const PILE_BUILD: usize = 16;
pub const PILE_MOMENTS: usize = 17;
const N: usize = 18;
const NAMES: [&str; N] = ["total", "resolve", "view", "clean", "take", "add_procs", "model", "packet", "signal",
                          "fire", "setup", "result", "presence", "heat", "pooled", "no_status", "pile_build", "pile_moments"];

static ENABLED: AtomicBool = AtomicBool::new(false);
/// 启用时需要计时的函数，默认选择全部。
static MASK: AtomicU64 = AtomicU64::new(u64::MAX);
static NANOS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

pub fn add(slot: usize, nanos: u64) {
    NANOS[slot].fetch_add(nanos, Ordering::Relaxed);
    CALLS[slot].fetch_add(1, Ordering::Relaxed);
}

pub fn enable(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// 根据名称选择计时项，减少细粒度计时对测量的影响。
pub fn select(names: &[String]) {
    let mut mask = 0u64;
    for (i, n) in NAMES.iter().enumerate() {
        if names.iter().any(|x| x == n) {
            mask |= 1 << i;
        }
    }
    MASK.store(if names.is_empty() { u64::MAX } else { mask }, Ordering::Relaxed);
}

/// 记录一次完整函数调用的计时器，停用时无操作。
pub struct Timer(usize, Option<Instant>);

impl Timer {
    #[inline]
    pub fn start(slot: usize) -> Timer {
        Timer(slot, if ENABLED.load(Ordering::Relaxed) && MASK.load(Ordering::Relaxed) & (1 << slot) != 0 { Some(Instant::now()) } else { None })
    }
}

impl Drop for Timer {
    #[inline]
    fn drop(&mut self) {
        if let Some(t) = self.1 {
            add(self.0, t.elapsed().as_nanos() as u64);
        }
    }
}

/// 返回各项名称、秒数和调用次数，`reset` 在读取后清零。
pub fn snapshot(reset: bool) -> Vec<(&'static str, f64, u64)> {
    (0..N)
        .map(|i| {
            let (n, c) = if reset {
                (NANOS[i].swap(0, Ordering::Relaxed), CALLS[i].swap(0, Ordering::Relaxed))
            } else {
                (NANOS[i].load(Ordering::Relaxed), CALLS[i].load(Ordering::Relaxed))
            };
            (NAMES[i], n as f64 * 1e-9, c)
        })
        .collect()
}

/// 按可选参数更新计时配置并读取计数。
pub fn configure_and_snapshot(reset: bool, enabled: Option<bool>, only: Option<Vec<String>>) -> Vec<(&'static str, f64, u64)> {
    if let Some(on) = enabled {
        enable(on);
    }
    if let Some(names) = only {
        select(&names);
    }
    snapshot(reset)
}
