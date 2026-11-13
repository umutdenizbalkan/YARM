// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-TIME-FOUNDATION1 B — the clock model, against an injected counter.

use super::*;
use crate::arch::clock::HOSTED_COUNTER;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    CLOCK_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
fn set_counter(v: u64) {
    HOSTED_COUNTER.store(v, Ordering::Release);
}

#[test]
fn b01_monotonic_reads_never_decrease() {
    let _g = lock();
    set_counter(5_000);
    force_source_for_test(1_000_000_000, 5_000, SourceKind::Hosted);
    let mut prev = 0;
    for step in [0u64, 1, 1, 7, 1_000, 1_000, 999_999] {
        set_counter(HOSTED_COUNTER.load(Ordering::Acquire) + step);
        let now = monotonic_ns().expect("source");
        assert!(now >= prev, "{now} < {prev}");
        prev = now;
    }
    assert_eq!(monotonic_ns_at(5_000), Ok(0), "the origin is zero");
}

#[test]
fn b02_counter_frequency_conversion_is_exact() {
    let _g = lock();
    assert_eq!(
        counter_delta_to_ns(62_500_000, 62_500_000),
        Some(1_000_000_000)
    );
    assert_eq!(
        counter_delta_to_ns(1, 62_500_000),
        Some(16),
        "16 ns per AArch64 QEMU tick (floor)"
    );
    assert_eq!(
        counter_delta_to_ns(10_000_000, 10_000_000),
        Some(1_000_000_000)
    );
    assert_eq!(counter_delta_to_ns(1, 10_000_000), Some(100));
    assert_eq!(counter_delta_to_ns(3, 3_000_000_000), Some(1));
    assert_eq!(counter_delta_to_ns(123, 0), None, "no frequency, no time");
    force_source_for_test(10_000_000, 0, SourceKind::RiscvTime);
    assert_eq!(resolution_ns(), Some(100));
    force_source_for_test(62_500_000, 0, SourceKind::Aarch64Cntpct);
    assert_eq!(resolution_ns(), Some(16));
    force_source_for_test(3_000_000_000, 0, SourceKind::X86TscPitCalibrated);
    assert_eq!(resolution_ns(), Some(1));
}

#[test]
fn b03_conversion_overflow_is_refused_not_wrapped() {
    let _g = lock();
    assert_eq!(counter_delta_to_ns(u64::MAX, 1), None);
    assert_eq!(counter_delta_to_ns(u64::MAX, NSEC_PER_SEC), Some(u64::MAX));
    // Large but representable: 2^63 counts at 10 GHz.
    assert_eq!(
        counter_delta_to_ns(1 << 63, 10_000_000_000),
        Some((1u64 << 63) / 10)
    );
    force_source_for_test(1, 0, SourceKind::Hosted);
    assert_eq!(monotonic_ns_at(u64::MAX), Err(ClockError::Overflow));
    assert_eq!(
        monotonic_ns_at(18_446_744_073),
        Ok(18_446_744_073_000_000_000)
    );
}

#[test]
fn b04_an_unsynchronized_source_is_clamped_across_cpus() {
    let _g = lock();
    force_source_for_test(1_000_000_000, 0, SourceKind::X86TscPitCalibrated);
    assert!(!SourceKind::X86TscPitCalibrated.cross_cpu_synchronized());
    // CPU A reads 10 µs; CPU B's TSC lags by 3 µs and reads afterwards.
    let a = monotonic_ns_at(10_000).expect("a");
    let b = monotonic_ns_at(7_000).expect("b");
    assert!(b >= a, "a lagging CPU never sees time go back: {b} < {a}");
    assert_eq!(b, a);
    assert_eq!(
        monotonic_ns_at(12_000),
        Ok(12_000),
        "and real time resumes once it catches up"
    );
}

#[test]
fn b05_a_shared_counter_needs_no_clamp_and_gets_none() {
    let _g = lock();
    for kind in [SourceKind::Aarch64Cntpct, SourceKind::RiscvTime] {
        force_source_for_test(1_000_000_000, 0, kind);
        assert!(kind.cross_cpu_synchronized());
        assert_eq!(monotonic_ns_at(10_000), Ok(10_000));
        assert_eq!(
            monotonic_ns_at(9_000),
            Ok(9_000),
            "{kind:?}: the raw shared count"
        );
        assert_eq!(LAST_MONOTONIC.load(Ordering::Acquire), 0, "no clamp state");
    }
}

#[test]
fn b06_time_advances_while_nothing_is_scheduled() {
    let _g = lock();
    set_counter(0);
    force_source_for_test(62_500_000, 0, SourceKind::Hosted);
    // No scheduler, no tick, no trap: only the counter moves.
    set_counter(62_500_000 * 3);
    assert_eq!(monotonic_ns(), Ok(3_000_000_000));
}

#[test]
fn b07_timespec_is_normalized() {
    let _g = lock();
    assert_eq!(
        Timespec::from_ns(1_999_999_999),
        Timespec {
            sec: 1,
            nsec: 999_999_999
        }
    );
    assert_eq!(Timespec::from_ns(0), Timespec { sec: 0, nsec: 0 });
    assert_eq!(
        Timespec::from_signed_ns(-1),
        Some(Timespec {
            sec: -1,
            nsec: 999_999_999
        })
    );
    assert_eq!(
        Timespec::from_signed_ns(-1_000_000_000),
        Some(Timespec { sec: -1, nsec: 0 })
    );
    for bad in [-1i64, 1_000_000_000, i64::MAX] {
        assert!(!Timespec { sec: 0, nsec: bad }.is_normalized());
        assert_eq!(Timespec { sec: 0, nsec: bad }.to_duration_ns(), None);
    }
    assert_eq!(
        Timespec { sec: -1, nsec: 0 }.to_duration_ns(),
        None,
        "a negative duration"
    );
    assert_eq!(
        Timespec {
            sec: i64::MAX,
            nsec: 0
        }
        .to_duration_ns(),
        None,
        "beyond u64 ns"
    );
    let t = Timespec {
        sec: 0x0102_0304_0506_0708,
        nsec: 999,
    };
    assert_eq!(
        Timespec::from_bytes(&t.to_bytes()),
        t,
        "the wire form round-trips"
    );
    assert_eq!(core::mem::size_of::<Timespec>(), 16);
    assert_eq!(core::mem::align_of::<Timespec>(), 8);
}

#[test]
fn b08_unknown_clock_ids_are_refused() {
    let _g = lock();
    assert_eq!(ClockId::from_raw(0), Some(ClockId::Realtime));
    assert_eq!(ClockId::from_raw(1), Some(ClockId::Monotonic));
    for raw in [2usize, 3, 4, 7, usize::MAX] {
        assert_eq!(ClockId::from_raw(raw), None, "clock {raw}");
    }
}

#[test]
fn b09_realtime_is_unavailable_until_set_then_follows_monotonic() {
    let _g = lock();
    set_counter(0);
    force_source_for_test(1_000_000_000, 0, SourceKind::Hosted);
    assert!(!realtime_initialized());
    assert_eq!(
        clock_gettime(ClockId::Realtime),
        Err(ClockError::Unavailable),
        "no epoch, no realtime"
    );
    assert!(
        clock_gettime(ClockId::Monotonic).is_ok(),
        "monotonic needs no epoch"
    );
    set_counter(5_000_000_000);
    let epoch = Timespec {
        sec: 1_763_000_000,
        nsec: 250,
    };
    set_realtime(epoch).expect("set");
    assert!(realtime_initialized());
    assert_eq!(
        clock_gettime(ClockId::Realtime),
        Ok(epoch),
        "exactly the value set, at that instant"
    );
    set_counter(7_500_000_000);
    assert_eq!(
        clock_gettime(ClockId::Realtime),
        Ok(Timespec {
            sec: 1_763_000_002,
            nsec: 500_000_250
        }),
        "and advances with the monotonic clock"
    );
    assert_eq!(
        set_realtime(Timespec { sec: -1, nsec: 0 }),
        Err(ClockError::Overflow),
        "pre-epoch refused"
    );
}

#[test]
fn b10_a_realtime_read_never_sees_a_half_published_offset() {
    let _g = lock();
    set_counter(0);
    force_source_for_test(1_000_000_000, 0, SourceKind::Hosted);
    set_realtime(Timespec { sec: 100, nsec: 0 }).expect("set");
    hold_realtime_write_for_test(true);
    assert_eq!(
        clock_gettime(ClockId::Realtime),
        Err(ClockError::Unavailable),
        "a writer in progress: bounded retries, then unavailable — never a torn value"
    );
    hold_realtime_write_for_test(false);
    assert_eq!(
        clock_gettime(ClockId::Realtime),
        Ok(Timespec { sec: 100, nsec: 0 })
    );
}

#[test]
fn b11_no_source_means_unavailable_never_a_guess() {
    let _g = lock();
    reset_for_test();
    assert_eq!(monotonic_ns(), Err(ClockError::Unavailable));
    assert_eq!(
        clock_gettime(ClockId::Monotonic),
        Err(ClockError::Unavailable)
    );
    assert_eq!(resolution_ns(), None);
    assert!(
        !install_source(0, 0, SourceKind::Hosted),
        "a zero frequency is refused"
    );
    assert!(install_source(1_000, 0, SourceKind::Hosted));
    assert!(
        !install_source(2_000, 0, SourceKind::Hosted),
        "the source is installed once"
    );
    assert_eq!(source_freq_hz(), 1_000);
}

#[test]
fn b12_a_second_realtime_writer_is_refused_never_spun() {
    let _g = lock();
    set_counter(0);
    force_source_for_test(1_000_000_000, 0, SourceKind::Hosted);
    hold_realtime_write_for_test(true);
    assert_eq!(
        set_realtime(Timespec { sec: 5, nsec: 0 }),
        Err(ClockError::Busy),
        "a write already in progress: the second writer returns at once"
    );
    hold_realtime_write_for_test(false);
    assert_eq!(set_realtime(Timespec { sec: 5, nsec: 0 }), Ok(()));
    assert_eq!(
        clock_gettime(ClockId::Realtime),
        Ok(Timespec { sec: 5, nsec: 0 })
    );
}
