// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
#[repr(align(64))]
struct CachePaddedFlag(AtomicBool);

/// QEMU-SMP3 witness only — acquisitions that found the lock HELD and waited for it, all locks,
/// all CPUs. This is contention actually measured, kept apart from operations that merely
/// overlapped in time. A spurious weak-CAS failure on a free lock is not counted: only an
/// observed holder is.
#[cfg(feature = "riscv64-smp3-witness")]
static WITNESS_CONTENDED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "riscv64-smp3-witness")]
pub fn witness_contended_acquisitions() -> u64 {
    WITNESS_CONTENDED.load(Ordering::Relaxed)
}

use crate::arch::irq_guard::{self, ArchIrqState};

#[inline]
fn irq_save() -> ArchIrqState {
    irq_guard::irq_save()
}

#[inline]
fn irq_restore(state: ArchIrqState) {
    irq_guard::irq_restore(state)
}

/// A simple TTAS spin lock.
///
/// This lock does **not** disable interrupts. Callers must ensure they do not
/// acquire it from an interrupt context that can preempt a holder on the same
/// CPU, otherwise self-deadlock is possible.
#[derive(Debug)]
pub struct SpinLock<T> {
    held: CachePaddedFlag,
    value: UnsafeCell<T>,
}

#[derive(Debug)]
pub struct SpinLockIrq<T> {
    held: CachePaddedFlag,
    value: UnsafeCell<T>,
    // QEMU-LOCK1: a stable, non-zero identity for the ONE witnessed subdomain lock (the VM
    // address-space lock). Every other `SpinLockIrq` keeps id 0 and is never recorded. The field
    // exists only under the witness feature, so the plain build's layout and acquisition path are
    // byte-for-byte unchanged.
    #[cfg(feature = "riscv64-lock1-witness")]
    witness_id: u32,
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLockIrq<T> {}
unsafe impl<T: Send> Sync for SpinLockIrq<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            held: CachePaddedFlag(AtomicBool::new(false)),
            value: UnsafeCell::new(value),
        }
    }

    #[must_use = "if unused, the lock is immediately released when the guard is dropped"]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        #[cfg(feature = "riscv64-smp3-witness")]
        let mut waited = false;
        // Use compare_exchange_weak in the retry loop: weak CAS may spuriously
        // fail on LL/SC architectures, but is typically cheaper than strong CAS.
        while self
            .held
            .0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.held.0.load(Ordering::Relaxed) {
                #[cfg(feature = "riscv64-smp3-witness")]
                {
                    waited = true;
                }
                spin_loop();
            }
            spin_loop();
        }
        #[cfg(feature = "riscv64-smp3-witness")]
        if waited {
            WITNESS_CONTENDED.fetch_add(1, Ordering::Relaxed);
        }
        SpinLockGuard {
            lock: self,
            _not_send: PhantomData,
        }
    }

    #[must_use = "if unused, the lock is immediately released when the guard is dropped"]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        if self
            .held
            .0
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinLockGuard {
                lock: self,
                _not_send: PhantomData,
            })
        } else {
            None
        }
    }

    /// Return a raw pointer to the inner value without acquiring the lock.
    ///
    /// Crate-private; only for boot code that cannot hold the lock across a
    /// non-returning call (e.g. ERET to user space).  The caller must ensure
    /// no concurrent lock holder exists.
    pub(crate) fn data_ptr(&self) -> *mut T {
        self.value.get()
    }
}

impl<T> SpinLockIrq<T> {
    pub const fn new(value: T) -> Self {
        Self {
            held: CachePaddedFlag(AtomicBool::new(false)),
            value: UnsafeCell::new(value),
            #[cfg(feature = "riscv64-lock1-witness")]
            witness_id: 0,
        }
    }

    /// QEMU-LOCK1: construct the ONE witnessed subdomain lock, tagged with a stable non-zero id.
    /// The acquisition algorithm is identical to [`Self::new`]'s lock; the id only selects which
    /// instance the (default-off) witness records.
    #[cfg(feature = "riscv64-lock1-witness")]
    pub const fn new_witnessed(value: T, witness_id: u32) -> Self {
        Self {
            held: CachePaddedFlag(AtomicBool::new(false)),
            value: UnsafeCell::new(value),
            witness_id,
        }
    }

    #[must_use = "if unused, the lock is immediately released when the guard is dropped"]
    pub fn lock(&self) -> SpinLockIrqGuard<'_, T> {
        // QEMU-LOCK1: `observed_held` becomes true the first time this acquisition sees the lock
        // already HELD by someone else — the actual failed acquisition the contention witness
        // records, derived from the atomic flag itself, not from elapsed time. Recorded once, and
        // only for the witnessed instance; the production algorithm below is unchanged.
        #[cfg(feature = "riscv64-lock1-witness")]
        let mut observed_held = false;
        loop {
            while self.held.0.load(Ordering::Relaxed) {
                #[cfg(feature = "riscv64-lock1-witness")]
                if self.witness_id != 0 && !observed_held {
                    observed_held = true;
                    crate::kernel::lock1_witness::note_contended(self.witness_id);
                }
                spin_loop();
            }

            let irq_state = irq_save();
            if self
                .held
                .0
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                #[cfg(feature = "riscv64-lock1-witness")]
                if self.witness_id != 0 {
                    crate::kernel::lock1_witness::note_acquired(self.witness_id);
                    // The default-off, bounded hold hook runs here — holding the lock, with
                    // supervisor interrupts masked — before the guard is handed back.
                    crate::kernel::lock1_witness::maybe_hold(self.witness_id);
                }
                return SpinLockIrqGuard {
                    lock: self,
                    irq_state,
                    _not_send: PhantomData,
                };
            }
            irq_restore(irq_state);
            spin_loop();
        }
    }
}

pub struct SpinLockIrqGuard<'a, T> {
    lock: &'a SpinLockIrq<T>,
    irq_state: ArchIrqState,
    _not_send: PhantomData<*const UnsafeCell<()>>,
}

impl<T> core::fmt::Debug for SpinLockIrqGuard<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpinLockIrqGuard").finish_non_exhaustive()
    }
}

impl<T> core::ops::Deref for SpinLockIrqGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> core::ops::DerefMut for SpinLockIrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinLockIrqGuard<'_, T> {
    fn drop(&mut self) {
        // QEMU-LOCK1: record the release of the witnessed lock BEFORE the atomic store makes it
        // acquirable, so the holder's "released" event is causally before any waiter's subsequent
        // acquisition. The store and the IRQ restore below are the unchanged production release.
        #[cfg(feature = "riscv64-lock1-witness")]
        if self.lock.witness_id != 0 {
            crate::kernel::lock1_witness::note_released(self.lock.witness_id);
        }
        self.lock.held.0.store(false, Ordering::Release);
        irq_restore(self.irq_state);
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    _not_send: PhantomData<*const UnsafeCell<()>>,
}

impl<T> core::fmt::Debug for SpinLockGuard<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpinLockGuard").finish_non_exhaustive()
    }
}

impl<T> core::ops::Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: Exclusive mutable access is serialized by `held`; successful
        // CAS establishes the only live guard for this lock and `held` stays
        // true for the guard lifetime. `UnsafeCell` provides interior
        // mutability behind `&SpinLock`.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> core::ops::DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: See deref safety note above. `&mut self` on the guard
        // guarantees unique access through this guard.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.held.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn try_lock_reflects_lock_state() {
        let lock = SpinLock::new(7usize);
        let _guard = lock.lock();
        assert!(lock.try_lock().is_none());
    }

    #[test]
    fn try_lock_succeeds_when_unheld() {
        let lock = SpinLock::new(11usize);
        let guard = lock.try_lock();
        assert!(guard.is_some());
    }

    #[test]
    fn lock_is_released_when_guard_drops() {
        let lock = SpinLock::new(1usize);
        {
            let _guard = lock.lock();
            assert!(lock.try_lock().is_none());
        }
        assert!(lock.try_lock().is_some());
    }

    #[test]
    fn nested_try_lock_returns_none() {
        let lock = SpinLock::new(3usize);
        let _guard = lock.try_lock().expect("first acquire");
        assert!(lock.try_lock().is_none());
    }

    #[test]
    fn lock_allows_shared_counter_updates() {
        let lock = SpinLock::new(0usize);
        static TICKS: AtomicUsize = AtomicUsize::new(0);

        {
            let mut guard = lock.lock();
            *guard += 1;
            TICKS.fetch_add(1, Ordering::SeqCst);
        }

        {
            let mut guard = lock.lock();
            *guard += 1;
        }

        assert_eq!(*lock.lock(), 2);
        assert_eq!(TICKS.load(Ordering::SeqCst), 1);
    }
}
