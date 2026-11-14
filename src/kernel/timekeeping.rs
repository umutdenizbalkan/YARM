// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-TIME-FOUNDATION1 B (TIMEKEEP1) — architecture-neutral timekeeping in nanoseconds.
//!
//! # Two clocks, one counter
//!
//! * **`CLOCK_MONOTONIC`** — nanoseconds since this kernel's clock ORIGIN: the hardware counter
//!   value sampled when [`install_source`] ran on the bootstrap CPU, before userspace exists. It
//!   is read from the port's free-running counter (x86_64 TSC calibrated against the 8254 PIT,
//!   AArch64 `CNTPCT_EL0` at `CNTFRQ_EL0`, RISC-V `time` at the device tree's
//!   `timebase-frequency`), so it advances while every CPU idles and is unrelated to the
//!   scheduler tick or any timer interrupt.
//! * **`CLOCK_REALTIME`** — UTC nanoseconds since the Unix epoch, ONLY once an authorized
//!   control-plane caller has installed the offset ([`set_realtime`], NR 19 `ClockSetRealtime`). Until then every
//!   read reports [`ClockError::Unavailable`]. The kernel never invents an epoch: a counter that
//!   started at boot is not calendar time.
//!
//! # Guarantees
//!
//! * The monotonic clock never moves backward as observed by any sequence of reads, on any CPU.
//!   AArch64's system counter and RISC-V's `time` are architecturally one count shared by every
//!   PE / hart. The x86_64 TSC is not proven synchronized across CPUs here (QEMU `qemu64` reports
//!   no invariant-TSC bit), so every x86_64 read is additionally clamped to the largest value
//!   returned so far ([`LAST_MONOTONIC`]): nondecreasing by construction; a CPU whose TSC lags
//!   returns the clamp until it catches up.
//! * Conversion is checked: `delta * 1e9 / freq` in 128 bits, refused ([`ClockError::Overflow`])
//!   if it would not fit `u64` (≈ 584 years of uptime).
//! * Resolution is `ceil(1e9 / freq)` ns ([`resolution_ns`]) — 1 ns for any counter at or above 1 GHz, 16 ns at
//!   62.5 MHz, 100 ns at 10 MHz. It is the counter's, not the scheduler tick's; a nanosecond
//!   field does not by itself claim nanosecond precision.
//!
//! # Not changed by this module
//!
//! FutexWait timeouts, IPC deadlines, reply deadlines and scheduler quanta keep their documented
//! scheduler-tick semantics (`SchedDomain` clock). This clock coexists with them; the only
//! conversion boundary is explicit ([`monotonic_ns`] is never consulted by those paths).

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};

pub const NSEC_PER_SEC: u64 = 1_000_000_000;

/// The YARM-native clock ids. The numbers follow the POSIX `<time.h>` values most libcs use, so
/// a future libc maps them one to one; nothing else about Linux is implied.
pub const CLOCK_REALTIME_ID: usize = 0;
pub const CLOCK_MONOTONIC_ID: usize = 1;

/// The `timespec` the clock syscalls exchange with userspace: 16 bytes, 8-aligned,
/// `#[repr(C)] { sec: i64, nsec: i64 }`. A valid value has `0 <= nsec < 1_000_000_000`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

pub const TIMESPEC_SIZE: usize = 16;
pub const TIMESPEC_ALIGN: usize = 8;
const _: () = assert!(core::mem::size_of::<Timespec>() == TIMESPEC_SIZE);
const _: () = assert!(core::mem::align_of::<Timespec>() == TIMESPEC_ALIGN);

impl Timespec {
    /// A non-negative nanosecond count, normalized.
    pub const fn from_ns(ns: u64) -> Self {
        Self {
            sec: (ns / NSEC_PER_SEC) as i64,
            nsec: (ns % NSEC_PER_SEC) as i64,
        }
    }

    /// A signed nanosecond count (realtime may precede the epoch), normalized so `nsec >= 0`.
    pub fn from_signed_ns(ns: i128) -> Option<Self> {
        let sec = ns.div_euclid(NSEC_PER_SEC as i128);
        let nsec = ns.rem_euclid(NSEC_PER_SEC as i128);
        Some(Self {
            sec: i64::try_from(sec).ok()?,
            nsec: nsec as i64,
        })
    }

    /// `true` when `nsec` is in `[0, 1e9)`.
    pub const fn is_normalized(&self) -> bool {
        self.nsec >= 0 && self.nsec < NSEC_PER_SEC as i64
    }

    /// The value as signed nanoseconds; `None` if not normalized or out of the i128 domain.
    pub fn to_signed_ns(&self) -> Option<i128> {
        if !self.is_normalized() {
            return None;
        }
        Some(self.sec as i128 * NSEC_PER_SEC as i128 + self.nsec as i128)
    }

    /// The value as a non-negative duration in nanoseconds; `None` for a negative, unnormalized or
    /// unrepresentable (`> u64::MAX` ns) value.
    pub fn to_duration_ns(&self) -> Option<u64> {
        if self.sec < 0 {
            return None;
        }
        u64::try_from(self.to_signed_ns()?).ok()
    }

    /// The wire form (little-endian, `sec` then `nsec`).
    pub fn to_bytes(&self) -> [u8; TIMESPEC_SIZE] {
        let mut out = [0u8; TIMESPEC_SIZE];
        out[..8].copy_from_slice(&self.sec.to_le_bytes());
        out[8..].copy_from_slice(&self.nsec.to_le_bytes());
        out
    }

    pub fn from_bytes(b: &[u8; TIMESPEC_SIZE]) -> Self {
        let mut sec = [0u8; 8];
        let mut nsec = [0u8; 8];
        sec.copy_from_slice(&b[..8]);
        nsec.copy_from_slice(&b[8..]);
        Self {
            sec: i64::from_le_bytes(sec),
            nsec: i64::from_le_bytes(nsec),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockId {
    Realtime,
    Monotonic,
}

impl ClockId {
    pub const fn from_raw(raw: usize) -> Option<Self> {
        match raw {
            CLOCK_REALTIME_ID => Some(Self::Realtime),
            CLOCK_MONOTONIC_ID => Some(Self::Monotonic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockError {
    /// Not a supported clock id.
    InvalidClock,
    /// The clock has no source: no hardware counter was installed (monotonic), or no authorized
    /// caller has set the epoch (realtime).
    Unavailable,
    /// The value does not fit the representation.
    Overflow,
    /// Another authorized writer is publishing the realtime offset right now. The second writer is
    /// refused (bounded) rather than made to wait.
    Busy,
}

/// What kind of counter backs the monotonic clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SourceKind {
    None = 0,
    /// x86_64 TSC, frequency measured against the 8254 PIT; cross-CPU order clamped.
    X86TscPitCalibrated = 1,
    /// AArch64 generic timer `CNTPCT_EL0` at `CNTFRQ_EL0` (one system counter for every PE).
    Aarch64Cntpct = 2,
    /// RISC-V `time` CSR at the device tree's `/cpus/timebase-frequency` (one count per platform).
    RiscvTime = 3,
    /// Hosted test counter.
    Hosted = 4,
}

impl SourceKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::X86TscPitCalibrated => "x86_tsc_pit_calibrated",
            Self::Aarch64Cntpct => "aarch64_cntpct",
            Self::RiscvTime => "riscv_time_csr",
            Self::Hosted => "hosted",
        }
    }
    /// Whether the counter is one count shared by every CPU (no clamp needed for ordering).
    pub const fn cross_cpu_synchronized(self) -> bool {
        matches!(self, Self::Aarch64Cntpct | Self::RiscvTime | Self::Hosted)
    }
    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::X86TscPitCalibrated,
            2 => Self::Aarch64Cntpct,
            3 => Self::RiscvTime,
            4 => Self::Hosted,
            _ => Self::None,
        }
    }
}

/// The installed counter frequency in Hz; 0 = no source.
static FREQ_HZ: AtomicU64 = AtomicU64::new(0);
/// The counter value at the origin.
static ORIGIN: AtomicU64 = AtomicU64::new(0);
static KIND: AtomicU8 = AtomicU8::new(SourceKind::None as u8);
/// The largest monotonic value returned so far (the cross-CPU clamp; used when the source is not
/// one shared count).
pub(crate) static LAST_MONOTONIC: AtomicU64 = AtomicU64::new(0);

/// `delta` counter units at `freq_hz` as nanoseconds, in 128-bit arithmetic. `None` for a zero
/// frequency or a result that does not fit `u64`.
pub fn counter_delta_to_ns(delta: u64, freq_hz: u64) -> Option<u64> {
    if freq_hz == 0 {
        return None;
    }
    u64::try_from(delta as u128 * NSEC_PER_SEC as u128 / freq_hz as u128).ok()
}

/// `ceil(1e9 / freq)`: the smallest nonzero step the clock can report.
pub fn resolution_ns() -> Option<u64> {
    let f = FREQ_HZ.load(Ordering::Acquire);
    (f != 0).then(|| NSEC_PER_SEC.div_ceil(f).max(1))
}

/// Install the monotonic source: `freq_hz` counts per second, origin = `origin` (the counter
/// value now). Called once, on the bootstrap CPU, before userspace. A second call is refused, so
/// the origin and frequency can never change under a running reader.
pub fn install_source(freq_hz: u64, origin: u64, kind: SourceKind) -> bool {
    if freq_hz == 0 || kind == SourceKind::None {
        return false;
    }
    if FREQ_HZ.load(Ordering::Acquire) != 0 {
        return false;
    }
    ORIGIN.store(origin, Ordering::Relaxed);
    KIND.store(kind as u8, Ordering::Relaxed);
    FREQ_HZ.store(freq_hz, Ordering::Release);
    true
}

pub fn source_kind() -> SourceKind {
    SourceKind::from_u8(KIND.load(Ordering::Acquire))
}

pub fn source_freq_hz() -> u64 {
    FREQ_HZ.load(Ordering::Acquire)
}

/// The monotonic clock from an explicit counter sample (the core both the live path and the
/// hosted tests use).
pub fn monotonic_ns_at(counter: u64) -> Result<u64, ClockError> {
    let f = FREQ_HZ.load(Ordering::Acquire);
    if f == 0 {
        return Err(ClockError::Unavailable);
    }
    let origin = ORIGIN.load(Ordering::Relaxed);
    let ns = counter_delta_to_ns(counter.saturating_sub(origin), f).ok_or(ClockError::Overflow)?;
    if source_kind().cross_cpu_synchronized() {
        Ok(ns)
    } else {
        // Nondecreasing across CPUs by construction: the result is never below a value any CPU
        // already returned.
        let prev = LAST_MONOTONIC.fetch_max(ns, Ordering::AcqRel);
        Ok(ns.max(prev))
    }
}

/// `CLOCK_MONOTONIC` now.
pub fn monotonic_ns() -> Result<u64, ClockError> {
    monotonic_ns_at(crate::arch::clock::counter())
}

// ── CLOCK_REALTIME: an offset over the monotonic clock, published under a sequence count ──
//
// `realtime = monotonic + offset`. The offset is written only by `set_realtime` (NR 19, an
// authorized caller), under an odd/even sequence: a reader retries a bounded number of times
// while a write is in progress, so it never returns an offset from one write with the
// initialization state of another.

static RT_SEQ: AtomicU64 = AtomicU64::new(0);
static RT_OFFSET_NS: AtomicI64 = AtomicI64::new(0);
static RT_INIT: AtomicBool = AtomicBool::new(false);
const RT_READ_RETRIES: usize = 64;

/// A coherent `(initialized, offset)` pair, or `None` if every bounded retry saw a write.
fn realtime_offset() -> Option<(bool, i64)> {
    for _ in 0..RT_READ_RETRIES {
        let s0 = RT_SEQ.load(Ordering::Acquire);
        if s0 & 1 == 1 {
            core::hint::spin_loop();
            continue;
        }
        let init = RT_INIT.load(Ordering::Acquire);
        let off = RT_OFFSET_NS.load(Ordering::Acquire);
        if RT_SEQ.load(Ordering::Acquire) == s0 {
            return Some((init, off));
        }
    }
    None
}

/// `CLOCK_REALTIME` at the monotonic instant `mono`.
pub fn realtime_at(mono: u64) -> Result<Timespec, ClockError> {
    match realtime_offset() {
        Some((true, off)) => {
            Timespec::from_signed_ns(mono as i128 + off as i128).ok_or(ClockError::Overflow)
        }
        Some((false, _)) => Err(ClockError::Unavailable),
        // A writer held the sequence through every bounded retry: report unavailable rather than
        // spin; the caller may retry.
        None => Err(ClockError::Unavailable),
    }
}

/// Read `clock` now, as one coherent value.
pub fn clock_gettime(clock: ClockId) -> Result<Timespec, ClockError> {
    let mono = monotonic_ns()?;
    match clock {
        ClockId::Monotonic => Ok(Timespec::from_ns(mono)),
        ClockId::Realtime => realtime_at(mono),
    }
}

/// Install `CLOCK_REALTIME = epoch` at the monotonic instant `mono` (the caller's authorization is
/// checked by the syscall). `epoch` must be normalized and not before the Unix epoch.
pub fn set_realtime_at(epoch: Timespec, mono: u64) -> Result<(), ClockError> {
    if epoch.sec < 0 {
        return Err(ClockError::Overflow);
    }
    let epoch_ns = epoch.to_signed_ns().ok_or(ClockError::Overflow)?;
    let off = i64::try_from(epoch_ns - mono as i128).map_err(|_| ClockError::Overflow)?;
    // The writer claims the sequence itself, even → odd, with ONE compare-and-swap: no second
    // lock, no spin. A concurrent writer (or one racing a stale even value) is refused.
    let even = RT_SEQ.load(Ordering::Acquire);
    if even & 1 == 1
        || RT_SEQ
            .compare_exchange(even, even + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Err(ClockError::Busy);
    }
    RT_OFFSET_NS.store(off, Ordering::Release);
    RT_INIT.store(true, Ordering::Release);
    RT_SEQ.store(even + 2, Ordering::Release);
    Ok(())
}

pub fn set_realtime(epoch: Timespec) -> Result<(), ClockError> {
    let mono = monotonic_ns()?;
    set_realtime_at(epoch, mono)
}

pub fn realtime_initialized() -> bool {
    realtime_offset().is_some_and(|(init, _)| init)
}

/// SCHED-TIME-FOUNDATION1 C (SLEEP1) — the `CLOCK_MONOTONIC` instant (ns) at which a sleep on
/// `clock` described by `ts` ends, for a sleep admitted at the monotonic instant `mono`.
///
/// * Relative (`absolute == false`): `mono + ts`, on either clock: a relative sleep measures
///   elapsed time, which a realtime change does not alter.
/// * Absolute `CLOCK_MONOTONIC`: `ts` itself.
/// * Absolute `CLOCK_REALTIME`: the monotonic instant at which realtime reaches `ts`, converted
///   ONCE here from one coherent realtime read at `mono` (`mono + (ts - realtime(mono))`). A later
///   `ClockSetRealtime` does not move an admitted deadline.
///
/// `CLOCK_REALTIME` requires an initialized realtime clock (else `Unavailable`), in both modes, so
/// a sleep is never measured against an epoch the kernel does not have. `ts` must be normalized
/// and not negative (`Overflow`, which the syscall reports as `InvalidArgs`). A deadline already
/// in the past is `mono` (the sleep is due at once); one beyond `u64` nanoseconds saturates to
/// `u64::MAX` (the sleep never ends on its own — about 584 years).
pub fn sleep_deadline_at(
    clock: ClockId,
    absolute: bool,
    ts: Timespec,
    mono: u64,
) -> Result<u64, ClockError> {
    if !ts.is_normalized() || ts.sec < 0 {
        return Err(ClockError::Overflow);
    }
    let ts_ns = ts.to_signed_ns().ok_or(ClockError::Overflow)?;
    let realtime_now = match clock {
        ClockId::Realtime => Some(
            realtime_at(mono)?
                .to_signed_ns()
                .ok_or(ClockError::Overflow)?,
        ),
        ClockId::Monotonic => None,
    };
    let target: i128 = match (absolute, realtime_now) {
        (false, _) => mono as i128 + ts_ns,
        (true, None) => ts_ns,
        (true, Some(rt)) => mono as i128 + (ts_ns - rt),
    };
    Ok(target.clamp(mono as i128, u64::MAX as i128) as u64)
}

/// Boot: install the port's counter (BSP, before userspace) and report it.
pub fn init_boot_clock() {
    match crate::arch::clock::discover_source() {
        Some(src) => {
            let ok = install_source(src.freq_hz, src.origin, src.kind);
            crate::yarm_log!(
                "TIMEKEEP_SOURCE source={} freq_hz={} res_ns={} cross_cpu_synchronized={} installed={}",
                src.kind.name(),
                src.freq_hz,
                resolution_ns().unwrap_or(0),
                u8::from(src.kind.cross_cpu_synchronized()),
                u8::from(ok)
            );
        }
        None => crate::yarm_log!(
            "TIMEKEEP_SOURCE source=none freq_hz=0 res_ns=0 cross_cpu_synchronized=0 installed=0"
        ),
    }
}

/// Hosted tests: reset every clock fact (the statics are process-global).
#[cfg(test)]
pub(crate) fn reset_for_test() {
    FREQ_HZ.store(0, Ordering::Release);
    ORIGIN.store(0, Ordering::Release);
    KIND.store(SourceKind::None as u8, Ordering::Release);
    LAST_MONOTONIC.store(0, Ordering::Release);
    RT_SEQ.store(0, Ordering::Release);
    RT_OFFSET_NS.store(0, Ordering::Release);
    RT_INIT.store(false, Ordering::Release);
}

/// Hosted tests: hold the sequence odd, as a writer mid-publication would.
#[cfg(test)]
pub(crate) fn hold_realtime_write_for_test(held: bool) {
    let s = RT_SEQ.load(Ordering::Acquire);
    if held == (s & 1 == 0) {
        RT_SEQ.fetch_add(1, Ordering::AcqRel);
    }
}

/// Hosted tests: install a source of a given kind directly (bypassing the once-only guard).
#[cfg(test)]
pub(crate) fn force_source_for_test(freq_hz: u64, origin: u64, kind: SourceKind) {
    reset_for_test();
    assert!(install_source(freq_hz, origin, kind));
}

/// Hosted tests: the clock statics are process-global; every clock test holds this.
#[cfg(test)]
pub(crate) static CLOCK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[path = "timekeeping_tests.rs"]
mod tests;
