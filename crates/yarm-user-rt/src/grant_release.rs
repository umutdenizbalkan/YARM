// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-LIFETIME1 — the one way a grant holder gives up its copy (NR 22 `CapRelinquish`), shared
//! by every holder on the file-grant path: initramfs (the source), VFS (the relay) and PM (the
//! recipient).
//!
//! The commit point is the first moment the holder KNOWS no transfer still needs its slot:
//!
//! * a relay (initramfs, the source; VFS, the relay) that delivered a grant keeps its copy until
//!   the descriptor the grant was made for is CLOSED ([`HeldGrants`]). The client closes only
//!   after it received the grant, so by then the client's capability exists — derived from this
//!   one, and surviving its release. (A delivered reply is not enough: the receiver's copy can
//!   still be in flight when the replier's NR 7 returns.) A client that never closes is reclaimed
//!   by VFS on exit, which closes the descriptor on its behalf.
//! * an undelivered reply, or a completion the holder rejected (the kernel installs a transferred
//!   capability before the reply is validated), has no downstream copy: released at once.
//! * PM, the recipient, after NR 29, successful or not: the child references nothing of the grant.
//!
//! Two answers from NR 22 are not final:
//!
//! * `WouldBlock` — a queued transfer still derives a receiver's capability from this slot. The
//!   slot is PARKED and retried at the holder's next request, never dropped.
//! * a full park list — counted (`overflow`), never silently forgotten; the slot stays held,
//!   exactly as it was before this pass.
//!
//! `InvalidCapability` is final and harmless: the slot is already gone. A CapId carries its slot's
//! generation, so a retried release can never reach a newer capability that reused the slot.

use crate::syscall::{CapRelinquishOutcome, SyscallError};

/// What one release attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseStep {
    /// The slot is gone; `reclaimed` = the object left the registry with it.
    Released { reclaimed: bool, reparented: u32 },
    /// The slot was already gone (a stale or retried id).
    AlreadyGone,
    /// A queued transfer still derives from it: parked for retry.
    Deferred,
    /// Parked list full: still held, counted.
    Overflow,
    /// Another refusal (the slot is not a releasable MemoryObject): still held.
    Refused(SyscallError),
}

/// Slots whose release was deferred, retried until they go.
#[derive(Debug, Clone)]
pub struct PendingReleases<const N: usize> {
    caps: [Option<u64>; N],
    /// Releases that could not even be parked.
    pub overflow: u64,
}

impl<const N: usize> Default for PendingReleases<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PendingReleases<N> {
    pub const fn new() -> Self {
        Self {
            caps: [None; N],
            overflow: 0,
        }
    }

    /// How many slots are parked.
    pub fn parked(&self) -> usize {
        self.caps.iter().flatten().count()
    }

    fn classify(result: Result<CapRelinquishOutcome, SyscallError>) -> ReleaseStep {
        match result {
            Ok(o) => ReleaseStep::Released {
                reclaimed: o.reclaimed,
                reparented: o.reparented,
            },
            Err(SyscallError::InvalidCapability) => ReleaseStep::AlreadyGone,
            Err(SyscallError::WouldBlock) => ReleaseStep::Deferred,
            Err(e) => ReleaseStep::Refused(e),
        }
    }

    /// Release `cap` now through `relinquish`, parking it if the kernel says not yet.
    pub fn release(
        &mut self,
        cap: u64,
        mut relinquish: impl FnMut(u64) -> Result<CapRelinquishOutcome, SyscallError>,
    ) -> ReleaseStep {
        match Self::classify(relinquish(cap)) {
            ReleaseStep::Deferred => {
                if self.caps.contains(&Some(cap)) {
                    return ReleaseStep::Deferred;
                }
                match self.caps.iter_mut().find(|c| c.is_none()) {
                    Some(slot) => {
                        *slot = Some(cap);
                        ReleaseStep::Deferred
                    }
                    None => {
                        self.overflow = self.overflow.saturating_add(1);
                        ReleaseStep::Overflow
                    }
                }
            }
            other => other,
        }
    }

    /// Retry every parked slot once; returns how many left the list.
    pub fn retry(
        &mut self,
        mut relinquish: impl FnMut(u64) -> Result<CapRelinquishOutcome, SyscallError>,
    ) -> usize {
        let mut settled = 0;
        for slot in self.caps.iter_mut() {
            let Some(cap) = *slot else { continue };
            match Self::classify(relinquish(cap)) {
                ReleaseStep::Deferred => {}
                _ => {
                    *slot = None;
                    settled += 1;
                }
            }
        }
        settled
    }
}

/// Grants a relay delivered and still holds, keyed by the descriptor they were made for, until
/// that descriptor closes. A full table is the caller's to settle (it releases at once).
#[derive(Debug, Clone)]
pub struct HeldGrants<K: Copy + Eq, const N: usize> {
    slots: [Option<(K, u64)>; N],
}

impl<K: Copy + Eq, const N: usize> HeldGrants<K, N> {
    pub const fn new() -> Self {
        Self { slots: [None; N] }
    }

    /// How many grants are held.
    pub fn held(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Hold `cap` for `key`; `false` when the table is full (nothing was recorded).
    pub fn hold(&mut self, key: K, cap: u64) -> bool {
        match self.slots.iter_mut().find(|s| s.is_none()) {
            Some(slot) => {
                *slot = Some((key, cap));
                true
            }
            None => false,
        }
    }

    /// The descriptor `key` closed: hand every grant held for it to `release`, and forget them.
    /// Returns how many there were.
    pub fn close(&mut self, key: K, mut release: impl FnMut(u64)) -> usize {
        let mut n = 0;
        for slot in self.slots.iter_mut() {
            if let Some((k, cap)) = *slot
                && k == key
            {
                *slot = None;
                release(cap);
                n += 1;
            }
        }
        n
    }

    /// Every held grant whose key matches `pred` (e.g. all of one backend instance).
    pub fn close_where(
        &mut self,
        mut pred: impl FnMut(K) -> bool,
        mut release: impl FnMut(u64),
    ) -> usize {
        let mut n = 0;
        for slot in self.slots.iter_mut() {
            if let Some((k, cap)) = *slot
                && pred(k)
            {
                *slot = None;
                release(cap);
                n += 1;
            }
        }
        n
    }
}

impl<K: Copy + Eq, const N: usize> Default for HeldGrants<K, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The production release: NR 22.
pub fn relinquish_now(cap: u64) -> Result<CapRelinquishOutcome, SyscallError> {
    crate::syscall::cap_relinquish(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Result<CapRelinquishOutcome, SyscallError> {
        Ok(CapRelinquishOutcome {
            reparented: 1,
            reclaimed: false,
        })
    }

    #[test]
    fn a_deferred_release_is_parked_and_retried_until_it_goes() {
        let mut p: PendingReleases<2> = PendingReleases::new();
        assert_eq!(
            p.release(7, |_| Err(SyscallError::WouldBlock)),
            ReleaseStep::Deferred
        );
        assert_eq!(
            p.release(7, |_| Err(SyscallError::WouldBlock)),
            ReleaseStep::Deferred
        );
        assert_eq!(p.parked(), 1, "parked once");
        assert_eq!(p.retry(|_| Err(SyscallError::WouldBlock)), 0);
        assert_eq!(p.retry(|_| ok()), 1);
        assert_eq!(p.parked(), 0);
    }

    #[test]
    fn a_held_grant_goes_when_its_descriptor_closes_and_only_then() {
        let mut h: HeldGrants<(u64, u64), 2> = HeldGrants::new();
        assert!(h.hold((1, 7), 70));
        assert!(h.hold((1, 8), 80));
        assert!(
            !h.hold((1, 9), 90),
            "full: nothing recorded, the caller releases at once"
        );
        let mut out = [0u64; 2];
        let mut n = 0;
        assert_eq!(
            h.close((1, 9), |c| {
                out[n] = c;
                n += 1
            }),
            0
        );
        assert_eq!(
            h.close((1, 7), |c| {
                out[n] = c;
                n += 1
            }),
            1
        );
        assert_eq!(&out[..n], &[70]);
        assert_eq!(h.close((1, 7), |_| panic!("released twice")), 0);
        assert_eq!(
            h.close_where(
                |(i, _)| i == 1,
                |c| {
                    out[n] = c;
                    n += 1
                }
            ),
            1
        );
        assert_eq!(&out[..n], &[70, 80]);
        assert_eq!(h.held(), 0);
    }

    #[test]
    fn a_gone_slot_is_final_and_a_full_list_is_counted() {
        let mut p: PendingReleases<1> = PendingReleases::new();
        assert_eq!(
            p.release(1, |_| Err(SyscallError::InvalidCapability)),
            ReleaseStep::AlreadyGone
        );
        assert_eq!(
            p.release(2, |_| Err(SyscallError::WouldBlock)),
            ReleaseStep::Deferred
        );
        assert_eq!(
            p.release(3, |_| Err(SyscallError::WouldBlock)),
            ReleaseStep::Overflow
        );
        assert_eq!(p.overflow, 1);
        // A slot that went away while parked leaves the list too.
        assert_eq!(p.retry(|_| Err(SyscallError::InvalidCapability)), 1);
        assert_eq!(
            p.release(4, |_| Err(SyscallError::WrongObject)),
            ReleaseStep::Refused(SyscallError::WrongObject)
        );
        assert_eq!(
            p.release(5, |_| ok()),
            ReleaseStep::Released {
                reclaimed: false,
                reparented: 1
            }
        );
    }
}
