use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, Ordering};

/// A simple TTAS spin lock.
///
/// This lock does **not** disable interrupts. Callers must ensure they do not
/// acquire it from an interrupt context that can preempt a holder on the same
/// CPU, otherwise self-deadlock is possible.
#[derive(Debug)]
pub struct SpinLock<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        // Use compare_exchange_weak in the retry loop: weak CAS may spuriously
        // fail on LL/SC architectures, but is typically cheaper than strong CAS.
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.held.load(Ordering::Relaxed) {
                spin_loop();
            }
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
        self.lock.held.store(false, Ordering::Release);
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
