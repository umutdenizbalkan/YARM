// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-BASELINE1 §3 — the controlled workload for the **timer contract** the strict x86_64
//! smoke checks.
//!
//! # What the check was for
//!
//! "timer IRQ + EOI + scheduler tick progression": the local timer interrupt is delivered,
//! acknowledged and re-armed, and the scheduler's tick advances across successive interrupts.
//! It never asked for a preemption — at the shipped quantum none occurs — and it did not care
//! whether the CPU was idle when the interrupt landed. What it counted, though, was
//! `TIMER_SPLIT_TICK_OK`, which the split route emits ONLY for an interrupt that lands while a
//! task is running. On an ordinary boot almost every interrupt lands in the idle halt instead
//! (`TIMER_SPLIT_IDLE_ADVANCE_COMMITTED`), so the count was whatever the scheduling happened to
//! produce — usually one — and the check failed or passed by accident.
//!
//! # What this provides instead
//!
//! A task that deliberately stays in userspace, computing, until the timer has interrupted it
//! several times. Every such interrupt is serviced by the production timer owner exactly as any
//! other is — tick, acknowledge, re-arm, settle — and the kernel's own marker names the task it
//! interrupted (`TIMER_SPLIT_TICK_OK ... current=<tid>`). Four callee-saved registers carry
//! distinct sentinels through the whole loop, so every interrupted-and-resumed instruction is
//! also a check that the task came back with its own register file.
//!
//! # Bounded, and it does not assert
//!
//! The loop runs for exactly `BUDGET_CYCLES` of TSC and then stops. That budget has to span
//! several timer periods: the boot programs a ~0.8 s LAPIC deadline, and the whole active boot
//! is shorter than one period, so a spin that stopped any earlier could finish before the first
//! interrupt ever arrived. The loop also counts TSC jumps larger than `GAP_CYCLES` and reports
//! them, but only as information: under TCG a host hiccup is the same size as an interrupt, so
//! the count is neither a stop condition nor evidence. The evidence is the kernel's attribution.
//! Like the other witnesses it emits markers and a seal and never panics.

/// A jump between two consecutive TSC reads this large means the task was not running for it —
/// an interrupt, or the host descheduling QEMU. Reported, not graded.
const GAP_CYCLES: u64 = 200_000;
/// 2^33 TSC cycles: ~4 s at the 2.1 GHz the QEMU hosts run at (TCG's TSC is the host's), about
/// five of the ~0.8 s LAPIC deadlines, and still more than two on a host twice as fast.
const BUDGET_CYCLES: u64 = 1 << 33;
const SENTINEL: u64 = 0x7153_0000_0000_005A;

/// Run the witness once. Emits a begin marker and one seal.
pub fn run_once() {
    crate::user_log!(
        "TIMER_CONTRACT_WITNESS_BEGIN budget_cycles={} gap_cycles={}",
        BUDGET_CYCLES,
        GAP_CYCLES
    );
    // SAFETY: the loop touches no memory; it reads the TSC and holds the sentinels in registers.
    let Some((gaps, elapsed, mask, checked)) = (unsafe {
        crate::syscall::spin_checking_callee_saved(BUDGET_CYCLES, GAP_CYCLES, SENTINEL)
    }) else {
        crate::user_log!("TIMER_CONTRACT_WITNESS_SEAL result=unsupported");
        return;
    };
    let all = if checked == 0 {
        0
    } else {
        (1u32 << checked) - 1
    };
    let context_ok = checked > 0 && mask == all;
    let result = if context_ok && elapsed >= BUDGET_CYCLES {
        "ok"
    } else {
        "fail"
    };
    crate::user_log!(
        "TIMER_CONTRACT_WITNESS_SEAL elapsed_cycles={} gaps={} context_ok={} mask=0x{:x} checked={} result={}",
        elapsed,
        gaps,
        u8::from(context_ok),
        mask,
        checked,
        result
    );
}
