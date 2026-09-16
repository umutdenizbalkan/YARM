// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-TIMER5 §1 — the **authenticated kernel-idle boundary**, and the two facts a port needs
//! before it may turn an interrupted kernel frame into a user return.
//!
//! # Why `current == None` is not the question
//!
//! Every port answers "is a user task running on this CPU?" with the scheduler's `current` slot,
//! and a timer that finds it empty has, up to now, been treated as an idle-boundary timer. That
//! inference is wrong in one direction that matters: `current` is also empty in kernel windows
//! that are not the idle boundary at all — the boot path before the first dispatch, the interval
//! between a terminal transition clearing `current` and the drain that settles it, and any
//! `-> !` kernel path on its way to a halt. Redirecting a frame taken in one of THOSE windows
//! into user mode would resume a task on a stack, and with a continuation, belonging to kernel
//! code that was in the middle of something.
//!
//! So the authorization is not derived from scheduler state. It is **published by the idle
//! primitive itself**, which is the only code that knows it has reached the boundary, and it is
//! consumed exactly once by the trap that interrupted it.
//!
//! ```text
//!   idle primitive            park(cpu, sp)       parked := true
//!   trap entry (shared bridge) take_parked(cpu)   parked := false  -> authorization for THIS trap
//!   drain resumes a task      commit_user_return  the arch tail owes a ring-3/EL0 return
//!   arch tail                 take_user_return    converts the hardware frame, exactly once
//! ```
//!
//! Three properties make the flag sufficient where `current == None` was not:
//!
//! * **It is set only by a non-returning primitive.** Each of `idle_halt_loop` (x86_64) and
//!   `idle_no_eret_loop` (AArch64) is `-> !` and parks immediately before its halt instruction.
//!   While the flag is set, the CPU is provably executing that halt loop and nothing else, so an
//!   interrupt taken with it set was taken AT the boundary — not merely while `current` happened
//!   to be empty.
//! * **It is consumed destructively, by the first trap to see it.** A second trap — a nested one,
//!   or the next one after we returned to kernel code rather than to user mode — finds it clear
//!   and is not authorized. The primitive re-publishes it only by parking again.
//! * **It says nothing about what to do.** Authorization is necessary, never sufficient: the
//!   trap must also be a timer that committed [`TimerIdleQueueAdvance`], and the canonical
//!   selection must actually yield a markable task. Any of the three missing means the CPU
//!   returns to its halt.
//!
//! [`TimerIdleQueueAdvance`]: crate::kernel::syscall_split::SplitDispatchDisposition
//!
//! # The stack anchor
//!
//! Both idle primitives are entered by DIVERGING from inside a trap handler — the post-lock
//! drains reach them after the broad guard is dropped, and the x86_64 trap tail reaches its own
//! after the epilogue. Whatever that call chain had pushed is abandoned, and the halt loop then
//! runs at that depth.
//!
//! Before U9-TIMER5 that cost nothing, because the boundary was terminal: a CPU that parked never
//! left except through another trap that parked again, and the growth was invisible. Giving the
//! boundary a user return makes it a CYCLE — park, take a timer, resume a task, block, park again
//! — and each turn of that cycle would otherwise leave one more abandoned frame below the last:
//!
//! ```text
//!   park @ S         trap (+F)      resume         block, park @ S-F-R        (drifts down)
//! ```
//!
//! The anchor removes the drift without inventing a stack. The FIRST park on a CPU records the
//! stack pointer it parked at; every later park resets to that same recorded value before
//! halting. The boundary therefore has ONE depth per CPU for the life of the boot, and the cycle
//! is flat rather than monotone. It is the CPU's own established kernel stack — not a new region,
//! not a per-task stack, and not a second allocation — and resetting to it is safe precisely
//! because both primitives diverge: nothing below the anchor is ever read again.
//!
//! x86_64 additionally re-enters the kernel through the TSS `RSP0` on the next trap from ring 3,
//! so its depth would have been re-based anyway; AArch64 keeps `SP_EL1` across an `eret` and has
//! no such mechanism, which is why the anchor is owned here, once, for both.
//!
//! # What this module deliberately does not do
//!
//! It holds no scheduler state, takes no lock, and makes no selection. The queue advance it
//! authorizes is performed by [`queue_advance_acquire_incoming_split`] over the same
//! `yield_dispatch_step_mut` step every other drain selects through, so no second scheduling
//! policy comes into existence here.
//!
//! [`queue_advance_acquire_incoming_split`]: crate::runtime::SharedKernel

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::kernel::scheduler::MAX_CPUS;

/// Set by the idle primitive immediately before its halt instruction; cleared by the first trap
/// that reads it.
static PARKED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// The one stack depth this CPU's idle boundary runs at, recorded at its first park. Zero means
/// "not yet anchored"; the first park installs its own stack pointer and every later park reuses
/// it. A zero stack pointer is not a legal anchor on either port, so zero is an unambiguous
/// sentinel rather than a value that could collide with a real stack.
static STACK_ANCHOR: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Published by the shared bridge when an authenticated idle-boundary timer has selected, marked
/// and restored an incoming task. It is a debt owed to the architecture tail — the tail is the
/// only code that can establish the hardware frame — and it is taken exactly once.
static USER_RETURN_OWED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// How many times each CPU has parked. Counted so a witness can show the boundary was genuinely
/// entered rather than inferred from the absence of a marker.
static PARK_COUNT: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

/// How many authenticated idle-boundary user returns this CPU has committed.
static USER_RETURN_COUNT: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

/// The LOWEST stack pointer ever observed at a park on this CPU. `u64::MAX` means "nothing
/// observed yet"; every park lowers it or leaves it alone.
///
/// This is the accumulation detector, and it detects the thing the anchor exists to prevent. The
/// anchor fixes the depth the halt loop RUNS at; what could still drift is the depth each new park
/// ARRIVES at, because every park is reached by diverging from inside a trap handler. If the cycle
/// leaked one vector frame per turn, successive arrivals would sit steadily lower and this value
/// would keep falling. A boot whose low-water mark settles after the first few parks and then
/// never moves again is a boot whose park → timer → resume → block → park cycle is flat.
static PARK_SP_LOW: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];

/// Publish the boundary. Called by the idle primitive with the stack pointer it is about to halt
/// on, and returns the anchor the primitive must switch to first.
///
/// The anchor is install-once per CPU: the returned value is this CPU's first-ever park depth,
/// which on the first call is `sp` itself. Both callers are `-> !`, so re-basing the stack to the
/// returned value discards only frames that can never be read again.
pub fn park(cpu: usize, sp: u64) -> ParkOutcome {
    if cpu >= MAX_CPUS {
        return ParkOutcome {
            anchor: sp,
            new_low: false,
        };
    }
    // `compare_exchange` rather than a load-then-store: two CPUs never share a slot, but the
    // install must still be one-shot against this CPU's own later parks.
    let anchor =
        match STACK_ANCHOR[cpu].compare_exchange(0, sp, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => sp,
            Err(existing) => existing,
        };
    let new_low = PARK_SP_LOW[cpu].fetch_min(sp, Ordering::AcqRel).gt(&sp);
    PARK_COUNT[cpu].fetch_add(1, Ordering::Relaxed);
    // Release-ordered LAST, so a trap that observes `parked` also observes the anchor install.
    PARKED[cpu].store(true, Ordering::Release);
    ParkOutcome { anchor, new_low }
}

/// What [`park`] tells its caller: where to halt, and whether this park arrived deeper than any
/// before it on this CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParkOutcome {
    /// The stack pointer the idle primitive must halt on — this CPU's install-once anchor.
    pub anchor: u64,
    /// True when this park set a new low-water mark. A flat cycle reports this a handful of times
    /// and then never again; a leaking one keeps reporting it.
    pub new_low: bool,
}

/// Consume this CPU's parked publication. Returns whether the trap calling it interrupted the
/// authenticated idle boundary, and clears the flag so no later trap can inherit the answer.
pub fn take_parked(cpu: usize) -> bool {
    if cpu >= MAX_CPUS {
        return false;
    }
    PARKED[cpu].swap(false, Ordering::AcqRel)
}

/// Read the flag without consuming it. Diagnostics and tests only — the authorization itself is
/// always taken through [`take_parked`], so it can never be spent twice.
pub fn is_parked(cpu: usize) -> bool {
    if cpu >= MAX_CPUS {
        return false;
    }
    PARKED[cpu].load(Ordering::Acquire)
}

/// This CPU's idle-boundary stack anchor, or 0 before its first park.
pub fn stack_anchor(cpu: usize) -> u64 {
    if cpu >= MAX_CPUS {
        return 0;
    }
    STACK_ANCHOR[cpu].load(Ordering::Acquire)
}

/// Record that an authenticated idle-boundary advance resumed a task, so the architecture tail
/// must return to user mode instead of to the halt it interrupted.
pub fn commit_user_return(cpu: usize) {
    if cpu >= MAX_CPUS {
        return;
    }
    USER_RETURN_COUNT[cpu].fetch_add(1, Ordering::Relaxed);
    USER_RETURN_OWED[cpu].store(true, Ordering::Release);
}

/// Take the debt. The architecture tail calls this exactly once per trap; a `true` answer means
/// this trap's hardware frame must be converted to a user frame.
pub fn take_user_return(cpu: usize) -> bool {
    if cpu >= MAX_CPUS {
        return false;
    }
    USER_RETURN_OWED[cpu].swap(false, Ordering::AcqRel)
}

/// How many times this CPU has entered its idle boundary.
pub fn park_count(cpu: usize) -> usize {
    if cpu >= MAX_CPUS {
        return 0;
    }
    PARK_COUNT[cpu].load(Ordering::Relaxed)
}

/// The lowest stack pointer observed at a park on this CPU, or `u64::MAX` before its first park.
pub fn park_sp_low(cpu: usize) -> u64 {
    if cpu >= MAX_CPUS {
        return u64::MAX;
    }
    PARK_SP_LOW[cpu].load(Ordering::Acquire)
}

/// How many authenticated idle-boundary user returns this CPU has committed.
pub fn user_return_count(cpu: usize) -> usize {
    if cpu >= MAX_CPUS {
        return 0;
    }
    USER_RETURN_COUNT[cpu].load(Ordering::Relaxed)
}

/// Test-only reset so cases can start from a known boundary state. Production never clears an
/// anchor: it is install-once for the life of the boot, which is what makes the depth constant.
#[cfg(test)]
pub fn reset_for_test(cpu: usize) {
    if cpu >= MAX_CPUS {
        return;
    }
    PARKED[cpu].store(false, Ordering::Release);
    USER_RETURN_OWED[cpu].store(false, Ordering::Release);
    STACK_ANCHOR[cpu].store(0, Ordering::Release);
    PARK_COUNT[cpu].store(0, Ordering::Relaxed);
    USER_RETURN_COUNT[cpu].store(0, Ordering::Relaxed);
    PARK_SP_LOW[cpu].store(u64::MAX, Ordering::Release);
}
