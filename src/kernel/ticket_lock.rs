// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK-FAIR1 — the IRQ-safe FIFO ticket lock of the scheduler domain.
//!
//! Used for exactly two lock kinds: every per-CPU scheduler lock (rank **1L**) and the admission
//! lock (rank **1G**) of [`crate::kernel::scheduler::SchedDomain`]. Every other kernel lock keeps
//! the TTAS [`crate::kernel::lock::SpinLockIrq`]. `doc/SCHEDULING_CONTRACT.md` §22.
//!
//! # Algorithm
//!
//! Two 32-bit counters in one cache line: `next` (the next ticket to hand out) and `serving` (the
//! ticket that owns the lock).
//!
//! * **lock** —
//!   1. save and mask local interrupts;
//!   2. draw `t = next.fetch_add(1)`, which publishes the ticket;
//!   3. spin, interrupts still masked, until `serving == t` (Acquire).
//! * **unlock** (the guard's drop) — store `serving = t + 1` (Release), then restore the saved
//!   interrupt state.
//! * **try_lock** —
//!   1. save and mask;
//!   2. read `s = serving` (Acquire);
//!   3. CAS `next` from `s` to `s + 1`.
//!
//!   The CAS succeeds only if no ticket was outstanding at `s` (`next == serving`: nobody holds
//!   and nobody waits). The caller then owns ticket `s`, which is already being served. Otherwise
//!   nothing was published, the interrupt state is restored, and `None` is returned. A try-lock
//!   therefore never takes a place ahead of a queued entrant and never leaves an unserved ticket.
//!
//! # Why interrupts are masked BEFORE the ticket is drawn, and stay masked while waiting
//!
//! A ticket, once drawn, must be served, and served by this context. Consider an interrupt taken
//! on this CPU between the draw and `serving == t`, whose handler takes the same lock. The handler
//! would draw `t' > t`. It would then spin forever behind ticket `t`, whose owner (the context it
//! interrupted) cannot run until the handler returns. So the draw happens with interrupts already
//! masked, and the wait keeps them masked.
//!
//! The masked wait adds no new dependency on this CPU taking an interrupt:
//! * kernel traps already run masked end to end (§6 W3);
//! * no holder of a 1L, 1G or a lock nested in them waits for an IPI acknowledgement (§22.3, A2).
//!
//! # Ticket wraparound
//!
//! The counters wrap modulo 2³². Only equality and the wrapping difference `t − serving` are ever
//! used, and both are exact as long as fewer than 2³² tickets are outstanding at once. At most one
//! ticket per CPU can be outstanding per lock:
//! * interrupts are masked from the draw to the release, so no interrupt can draw a second one;
//! * no code path re-acquires a scheduler lock it holds (the lock order forbids it, and it would
//!   self-deadlock under any lock).
//!
//! So at most `MAX_CPUS` = 64 tickets are outstanding.
//!
//! The try-lock's read-then-CAS has the usual ABA window: `next` would have to advance by exactly
//! 2³² between the read and the CAS, while the trying CPU runs with interrupts masked. That is
//! assumption A4 in §22.3.
//!
//! # Ordering
//!
//! * The Acquire load that observes `serving == t` synchronizes with the previous holder's Release
//!   store of `serving = t`, so its whole section happens-before this one.
//! * The draw is a Relaxed RMW: uniqueness and order of tickets come from the RMW's atomicity on
//!   `next` (its modification order), and no data is published through `next`.
//! * The try-lock's Acquire read of `serving` gives the same synchronization. The CAS on `next`
//!   only proves that no later ticket was drawn in between.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::irq_guard::{self, ArchIrqState};

/// The two counters, alone in one cache line — the same footprint as the TTAS lock's padded flag.
#[derive(Debug)]
#[repr(align(64))]
struct Tickets {
    next: AtomicU32,
    serving: AtomicU32,
}

/// Rank 1L / 1G: an interrupt-masking FIFO ticket lock. See the module documentation.
#[derive(Debug)]
pub struct TicketLockIrq<T> {
    tickets: Tickets,
    value: UnsafeCell<T>,
    /// SCHED-LOCK1 witness only: which scheduler lock this is (a CPU index, or `FAIR_ID_1G`), so
    /// the fair-admission witness can attribute each acquisition. Absent from the default build.
    #[cfg(feature = "sched-lock1-witness")]
    fair_id: u8,
}

// SAFETY: the lock serializes every access to `value`; the guard is the only path to it.
unsafe impl<T: Send> Send for TicketLockIrq<T> {}
unsafe impl<T: Send> Sync for TicketLockIrq<T> {}

/// The witness id of the admission lock (1G). CPU cells use their index, `< MAX_CPUS`.
#[cfg(feature = "sched-lock1-witness")]
pub(crate) const FAIR_ID_1G: u8 = 0xFF;

#[cfg(not(test))]
#[inline]
fn irq_save() -> ArchIrqState {
    irq_guard::irq_save()
}

#[cfg(not(test))]
#[inline]
fn irq_restore(state: ArchIrqState) {
    irq_guard::irq_restore(state)
}

/// Hosted-test IRQ model: the hosted `irq_save` is a no-op, so tests could not otherwise observe
/// that interrupts are masked before a ticket is drawn. A per-thread "enabled" flag mirrors what
/// the hardware save/restore does; the hardware path itself is unchanged.
#[cfg(test)]
pub(crate) mod test_irq {
    use core::cell::Cell;
    std::thread_local! {
        pub(crate) static ENABLED: Cell<bool> = const { Cell::new(true) };
        /// The simulated state each ticket draw on this thread saw (true = enabled: a violation).
        pub(crate) static DRAWS_WITH_IRQS_ENABLED: Cell<u32> = const { Cell::new(0) };
        /// Every spin iteration observed by this thread while waiting, and how many of them ran
        /// with the simulated interrupts enabled (a violation).
        pub(crate) static WAIT_SPINS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static WAIT_SPINS_IRQS_ENABLED: Cell<u64> = const { Cell::new(0) };
        /// The entrants this thread found ahead of its last drawn ticket (`t - serving`).
        pub(crate) static LAST_AHEAD: Cell<u32> = const { Cell::new(0) };
    }
    pub(crate) fn enabled() -> bool {
        ENABLED.with(Cell::get)
    }
    pub(crate) fn set_enabled(on: bool) {
        ENABLED.with(|c| c.set(on));
    }
}

#[cfg(test)]
fn irq_save() -> ArchIrqState {
    let mut state = irq_guard::irq_save();
    state.interrupts_were_enabled = test_irq::ENABLED.with(|c| c.replace(false));
    state
}

#[cfg(test)]
fn irq_restore(state: ArchIrqState) {
    test_irq::ENABLED.with(|c| c.set(state.interrupts_were_enabled));
    irq_guard::irq_restore(state)
}

impl<T> TicketLockIrq<T> {
    pub const fn new(value: T) -> Self {
        Self {
            tickets: Tickets {
                next: AtomicU32::new(0),
                serving: AtomicU32::new(0),
            },
            value: UnsafeCell::new(value),
            #[cfg(feature = "sched-lock1-witness")]
            fair_id: 0,
        }
    }

    /// SCHED-LOCK1 witness only: a lock tagged for the fair-admission witness.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) const fn new_fair(value: T, fair_id: u8) -> Self {
        let mut lock = Self::new(value);
        lock.fair_id = fair_id;
        lock
    }

    /// Hosted tests only: start both counters at `base`, so wraparound is reached in a few
    /// acquisitions.
    #[cfg(test)]
    pub(crate) fn with_ticket_base(value: T, base: u32) -> Self {
        let lock = Self::new(value);
        lock.tickets.next.store(base, Ordering::Relaxed);
        lock.tickets.serving.store(base, Ordering::Relaxed);
        lock
    }

    /// Exclusive access needs no lock: `&mut self` proves no guard exists.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// Is the lock held or awaited right now (by anyone)? An observation, never a decision.
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub fn is_locked(&self) -> bool {
        self.tickets.next.load(Ordering::Acquire) != self.tickets.serving.load(Ordering::Acquire)
    }

    /// Tickets drawn and not yet released: the holder plus every waiter (wrap-safe difference).
    /// An observation for tests and the witness; it decides nothing.
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub fn outstanding(&self) -> u32 {
        let serving = self.tickets.serving.load(Ordering::Acquire);
        self.tickets
            .next
            .load(Ordering::Acquire)
            .wrapping_sub(serving)
    }

    /// The ticket currently being served (tests and the witness).
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub fn serving(&self) -> u32 {
        self.tickets.serving.load(Ordering::Acquire)
    }

    #[must_use = "if unused, the lock is immediately released when the guard is dropped"]
    pub fn lock(&self) -> TicketLockIrqGuard<'_, T> {
        // (1) Mask BEFORE the ticket is published: no interrupt on this CPU can queue behind the
        // context it interrupted.
        let irq_state = irq_save();
        // (2) Draw — this publishes the ticket.
        #[cfg(test)]
        if test_irq::enabled() {
            test_irq::DRAWS_WITH_IRQS_ENABLED.with(|c| c.set(c.get() + 1));
        }
        let ticket = self.tickets.next.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        test_irq::LAST_AHEAD
            .with(|c| c.set(ticket.wrapping_sub(self.tickets.serving.load(Ordering::Relaxed))));
        #[cfg(feature = "sched-lock1-witness")]
        let ahead = ticket.wrapping_sub(self.tickets.serving.load(Ordering::Relaxed));
        // (3) Wait, interrupts still masked, for this ticket to be served.
        while self.tickets.serving.load(Ordering::Acquire) != ticket {
            #[cfg(test)]
            {
                test_irq::WAIT_SPINS.with(|c| c.set(c.get() + 1));
                if test_irq::enabled() {
                    test_irq::WAIT_SPINS_IRQS_ENABLED.with(|c| c.set(c.get() + 1));
                }
            }
            spin_loop();
        }
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::fair_acquired(self.fair_id, ticket, ahead);
        TicketLockIrqGuard {
            lock: self,
            ticket,
            irq_state,
            _not_send: PhantomData,
        }
    }

    /// One attempt that never queues: succeeds only when nobody holds or awaits the lock, and then
    /// owns the ticket already being served. A failure publishes nothing. See the module doc.
    #[must_use = "if unused, the lock is immediately released when the guard is dropped"]
    pub fn try_lock(&self) -> Option<TicketLockIrqGuard<'_, T>> {
        let irq_state = irq_save();
        let serving = self.tickets.serving.load(Ordering::Acquire);
        if self
            .tickets
            .next
            .compare_exchange(
                serving,
                serving.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            #[cfg(feature = "sched-lock1-witness")]
            crate::kernel::schedlock1_witness::fair_acquired(self.fair_id, serving, 0);
            return Some(TicketLockIrqGuard {
                lock: self,
                ticket: serving,
                irq_state,
                _not_send: PhantomData,
            });
        }
        irq_restore(irq_state);
        None
    }
}

/// A held [`TicketLockIrq`]: owns ticket `ticket` and the interrupt state saved before drawing it.
pub struct TicketLockIrqGuard<'a, T> {
    lock: &'a TicketLockIrq<T>,
    ticket: u32,
    irq_state: ArchIrqState,
    _not_send: PhantomData<*const UnsafeCell<()>>,
}

impl<T> TicketLockIrqGuard<'_, T> {
    /// The ticket this guard holds (tests and the witness).
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub fn ticket(&self) -> u32 {
        self.ticket
    }

    /// The witness id of the lock this guard holds.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn fair_id(&self) -> u8 {
        self.lock.fair_id
    }
}

impl<T> core::fmt::Debug for TicketLockIrqGuard<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TicketLockIrqGuard")
            .field("ticket", &self.ticket)
            .finish_non_exhaustive()
    }
}

impl<T> core::ops::Deref for TicketLockIrqGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: this guard owns the served ticket, so no other reference to `value` exists.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> core::ops::DerefMut for TicketLockIrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: as for `deref`, exclusively.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for TicketLockIrqGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::fair_released(
            self.lock.fair_id,
            self.ticket,
            self.lock
                .tickets
                .next
                .load(Ordering::Relaxed)
                .wrapping_sub(self.ticket.wrapping_add(1)),
        );
        // Only the owner of `serving`'s ticket writes `serving`: hand the lock to the next ticket.
        self.lock
            .tickets
            .serving
            .store(self.ticket.wrapping_add(1), Ordering::Release);
        irq_restore(self.irq_state);
    }
}
