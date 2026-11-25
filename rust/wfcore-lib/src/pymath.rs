//! 参考引擎使用的 Python 浮点语义。
//!
//! `PySum` 复现 CPython 3.12 及以上的浮点 `sum()`。先以整数零加首项，再按 Neumaier 算法累积，最后加上有限的补偿值。
//! 超越函数与 CPython 的 `math` 和 `float.__pow__` 一样调用 C 运行库，保持结果逐位一致。

extern "C" {
    #[link_name = "log"]
    fn c_log(x: f64) -> f64;
    #[link_name = "expm1"]
    fn c_expm1(x: f64) -> f64;
    #[link_name = "erfc"]
    fn c_erfc(x: f64) -> f64;
    #[link_name = "pow"]
    fn c_pow(x: f64, y: f64) -> f64;
}

/// 引擎传入正数时对应 `math.log(x)`。
#[inline]
pub fn log(x: f64) -> f64 {
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    unsafe { c_log(x) }
}

#[inline]
pub fn expm1(x: f64) -> f64 {
    unsafe { c_expm1(x) }
}

#[inline]
pub fn erfc(x: f64) -> f64 {
    unsafe { c_erfc(x) }
}

/// 浮点 `x ** y` 先处理 CPython 的有限值特例，再调用 C 的 pow。
#[inline]
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    unsafe { c_pow(x, y) }
}

#[inline]
pub fn sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// 对引擎中的有限数值向下取整。
#[inline]
pub fn floor_i(x: f64) -> i64 {
    x.floor() as i64
}

/// 向上取整。
#[inline]
pub fn ceil_i(x: f64) -> i64 {
    x.ceil() as i64
}

/// 对应 `int(x)` 向零截断。
#[inline]
pub fn trunc_i(x: f64) -> i64 {
    x.trunc() as i64
}

/// Python 的 `max(a, b)` 仅在第二项更大时选第二项。
#[inline]
pub fn pmax(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Python 的 `min(a, b)` 仅在第二项更小时选第二项。
#[inline]
pub fn pmin(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// 以整数零为起点，按 CPython 的 `sum()` 方式累加浮点序列。
#[derive(Clone, Copy, Debug)]
pub struct PySum {
    started: bool,
    total: f64,
    comp: f64,
}

impl Default for PySum {
    fn default() -> Self {
        Self::new()
    }
}

impl PySum {
    #[inline]
    pub fn new() -> Self {
        PySum { started: false, total: 0.0, comp: 0.0 }
    }

    #[inline]
    pub fn add(&mut self, x: f64) {
        if !self.started {
            // 用整数零加当前浮点值。
            self.total = 0.0 + x;
            self.started = true;
            return;
        }
        let t = self.total + x;
        if self.total.abs() >= x.abs() {
            self.comp += (self.total - t) + x;
        } else {
            self.comp += (x - t) + self.total;
        }
        self.total = t;
    }

    #[inline]
    pub fn value(&self) -> f64 {
        if !self.started {
            return 0.0;
        }
        if self.comp != 0.0 && self.comp.is_finite() {
            self.total + self.comp
        } else {
            self.total
        }
    }
}

/// 对浮点迭代器执行 `sum(iterable)`。
#[inline]
pub fn psum<I: IntoIterator<Item = f64>>(items: I) -> f64 {
    let mut s = PySum::new();
    for x in items {
        s.add(x);
    }
    s.value()
}

/// 按接口名称调用对应的数学函数。
pub fn c_math(name: &str, x: f64, y: f64) -> f64 {
    match name {
        "log" => log(x),
        "expm1" => expm1(x),
        "erfc" => erfc(x),
        "pow" => pow(x, y),
        _ => f64::NAN,
    }
}
