// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! NOTIF-LIFETIME1 — capability-reference lifetime for Notification objects.
//!
//! # Contract
//!
//! A Notification incarnation `{index, generation}` exists while at least one capability naming
//! that exact incarnation is live. Removing a capability relinquishes ITS reference; the object
//! is destroyed only when the last reference goes. A capability naming any other generation of
//! the slot has no effect on the incarnation that occupies it now.
//!
//! # Accounting
//!
//! `IpcSubsystem::notification_refs[index]` counts the references held on the incarnation whose
//! generation is `notification_generations[index]`. It lives with the object, at IPC rank 3, and
//! is changed only through the two bodies below:
//!
//! * [`reserve_notification_ref_locked`] — taken at rank 3 BEFORE the capability is inserted at
//!   rank 4 (the same "pre-bump then install" order `cap_memory_mint_split` uses for memory
//!   objects). It refuses an incarnation that is gone, has another generation, or has reached
//!   zero: zero is terminal, so a reference can never resurrect a destroyed object.
//! * [`release_notification_refs_locked`] — given back at rank 3 AFTER the capability was removed
//!   at rank 4. It is THE destruction owner: the generation comparison, the decrement and, at
//!   zero, the route teardown, waiter retirement, object removal and the single generation bump
//!   all happen in one rank-3 critical section.
//!
//! Because a reference is always taken before its capability becomes visible and given back only
//! after it has gone, the count is never lower than the number of visible capabilities, so no
//! interleaving of insertions and removals can destroy a referenced incarnation. The two ranks are
//! never held together: rank 3 is released before rank 4 is taken, and vice versa.
//!
//! A creation holds one reference of its own while it mints, then gives it back
//! ([`KernelState::create_notification`]), so the object cannot vanish under its first mints and
//! a creation that mints nothing destroys what it made.

use super::{EndpointWaiterRecord, IpcSubsystem, KernelError, KernelState, MAX_NOTIFICATIONS};
use crate::kernel::capabilities::{CapObject, Capability};

/// The exact incarnation a Notification capability names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotificationIncarnation {
    pub(crate) index: usize,
    pub(crate) generation: u64,
}

impl NotificationIncarnation {
    pub(crate) fn of(object: CapObject) -> Option<Self> {
        match object {
            CapObject::Notification { index, generation } => Some(Self { index, generation }),
            _ => None,
        }
    }
}

/// The references a rank-4 removal took out of capability spaces, owed back at rank 3.
///
/// Aggregated per incarnation. A cspace derivation subtree always names ONE object (a derived
/// capability names its parent's object), so a revoke needs the default two entries; only a
/// whole-cspace drop can name many incarnations, and it sizes the list to the slot count
/// (`NotificationRefRelease<MAX_NOTIFICATIONS>`). Filling it needs nothing but the removed
/// capabilities themselves, so a removal records into it inside its own rank-4 section.
pub(crate) struct NotificationRefRelease<const N: usize = 2> {
    /// `(index, references, generation)`; `references == 0` marks an empty entry.
    entries: [(u16, u32, u64); N],
    /// Distinct incarnations that did not fit. Fail-safe: an unrecorded reference is leaked, never
    /// given back twice, so a full list can keep an object alive but can never destroy one.
    overflowed: u32,
}

impl<const N: usize> NotificationRefRelease<N> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: [(0, 0, 0); N],
            overflowed: 0,
        }
    }

    /// Record one removed capability. Anything but a Notification owes nothing.
    pub(crate) fn note_object(&mut self, object: CapObject) {
        let Some(incarnation) = NotificationIncarnation::of(object) else {
            return;
        };
        let Ok(index) = u16::try_from(incarnation.index) else {
            self.overflowed = self.overflowed.saturating_add(1);
            return;
        };
        for entry in self.entries.iter_mut() {
            if entry.1 != 0 && entry.0 == index && entry.2 == incarnation.generation {
                entry.1 = entry.1.saturating_add(1);
                return;
            }
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.1 == 0) {
            *entry = (index, 1, incarnation.generation);
            return;
        }
        self.overflowed = self.overflowed.saturating_add(1);
        crate::yarm_log!(
            "NOTIF_REF_RELEASE_FULL index={} generation={} capacity={}",
            incarnation.index,
            incarnation.generation,
            N
        );
    }

    pub(crate) fn note(&mut self, capability: &Capability) {
        self.note_object(capability.object);
    }

    fn iter(&self) -> impl Iterator<Item = (NotificationIncarnation, u32)> + '_ {
        self.entries
            .iter()
            .filter(|entry| entry.1 != 0)
            .map(|entry| {
                (
                    NotificationIncarnation {
                        index: usize::from(entry.0),
                        generation: entry.2,
                    },
                    entry.1,
                )
            })
    }
}

/// The release list for dropping a whole capability space: one entry per notification slot.
pub(crate) type CspaceNotificationRefs = NotificationRefRelease<MAX_NOTIFICATIONS>;

impl<const N: usize> Default for NotificationRefRelease<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// What one release did to the slot it named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotificationReleaseOutcome {
    /// The slot holds no incarnation, or a different one: nothing changed.
    Ignored,
    /// References remain; the object, its route and its waiter are untouched.
    Released { remaining: u32 },
    /// That was the last reference: the incarnation is gone. The waiter it retired, if any, is
    /// owed a wake once rank 3 is released.
    Destroyed {
        waiter: Option<EndpointWaiterRecord>,
    },
}

/// Rank 3: take one reference on `incarnation`, or refuse.
pub(crate) fn reserve_notification_ref_locked(
    ipc: &mut IpcSubsystem,
    incarnation: NotificationIncarnation,
) -> Result<(), KernelError> {
    let i = incarnation.index;
    if i >= MAX_NOTIFICATIONS || ipc.notifications[i].is_none() {
        return Err(KernelError::WrongObject);
    }
    if ipc.notification_generations[i] != incarnation.generation || ipc.notification_refs[i] == 0 {
        return Err(KernelError::StaleCapability);
    }
    ipc.notification_refs[i] = ipc.notification_refs[i]
        .checked_add(1)
        .ok_or(KernelError::CapabilityFull)?;
    Ok(())
}

/// Rank 3: give back `count` references on `incarnation` — THE destruction owner.
///
/// A generation mismatch, an empty slot or a slot already at zero is a complete no-op: no
/// decrement, no route or waiter change, no generation bump. At zero the incarnation is torn
/// down in this same critical section: every IRQ route naming the slot is cleared (a route is
/// bound to the incarnation that occupied the slot when it was bound, and a reused slot starts
/// with none), the waiter is taken, the object is removed and the generation advances once.
pub(crate) fn release_notification_refs_locked(
    ipc: &mut IpcSubsystem,
    incarnation: NotificationIncarnation,
    count: u32,
) -> NotificationReleaseOutcome {
    let i = incarnation.index;
    if count == 0
        || i >= MAX_NOTIFICATIONS
        || ipc.notifications[i].is_none()
        || ipc.notification_generations[i] != incarnation.generation
        || ipc.notification_refs[i] == 0
    {
        return NotificationReleaseOutcome::Ignored;
    }
    if count > ipc.notification_refs[i] {
        // More references given back than were taken: some removal was counted twice. Keep the
        // object rather than destroy what live capabilities may still name.
        crate::yarm_log!(
            "NOTIF_REF_OVER_RELEASE index={} generation={} held={} released={}",
            i,
            incarnation.generation,
            ipc.notification_refs[i],
            count
        );
        return NotificationReleaseOutcome::Ignored;
    }
    ipc.notification_refs[i] -= count;
    if ipc.notification_refs[i] > 0 {
        return NotificationReleaseOutcome::Released {
            remaining: ipc.notification_refs[i],
        };
    }
    for route in ipc.irq_routes.iter_mut() {
        if *route == Some(i) {
            *route = None;
        }
    }
    let waiter = ipc.notification_waiters[i].take();
    ipc.notifications[i] = None;
    let mut next_gen = ipc.notification_generations[i].wrapping_add(1);
    if next_gen == 0 {
        next_gen = 1;
    }
    ipc.notification_generations[i] = next_gen;
    NotificationReleaseOutcome::Destroyed { waiter }
}

impl KernelState {
    /// Broad: reserve a reference for a capability about to be inserted. Non-Notification
    /// objects need none.
    pub(crate) fn reserve_notification_ref(
        &mut self,
        object: CapObject,
    ) -> Result<(), KernelError> {
        let Some(incarnation) = NotificationIncarnation::of(object) else {
            return Ok(());
        };
        self.with_ipc_state_mut(|ipc| reserve_notification_ref_locked(ipc, incarnation))
    }

    /// Broad: give back the reference of one capability that was removed or never inserted.
    pub(crate) fn release_notification_ref(&mut self, object: CapObject) {
        let mut release: NotificationRefRelease = NotificationRefRelease::new();
        release.note_object(object);
        self.release_notification_refs(&release);
    }

    /// Broad: give back every reference a removal recorded, then wake each retired waiter with
    /// rank 3 released.
    pub(crate) fn release_notification_refs<const N: usize>(
        &mut self,
        release: &NotificationRefRelease<N>,
    ) {
        for (incarnation, count) in release.iter() {
            let outcome = self.with_ipc_state_mut(|ipc| {
                release_notification_refs_locked(ipc, incarnation, count)
            });
            if let NotificationReleaseOutcome::Destroyed {
                waiter: Some(record),
            } = outcome
            {
                let _ = self.wake_destroyed_notification_waiter(record);
            }
        }
    }
}

impl crate::runtime::SharedKernel {
    /// Split: [`KernelState::reserve_notification_ref`] through the rank-3 split seam.
    pub(crate) fn reserve_notification_ref_split(
        &self,
        object: CapObject,
    ) -> Result<(), KernelError> {
        let Some(incarnation) = NotificationIncarnation::of(object) else {
            return Ok(());
        };
        self.with_ipc_split_mut(|ipc| reserve_notification_ref_locked(ipc, incarnation))
    }

    /// Split: [`KernelState::release_notification_ref`].
    pub(crate) fn release_notification_ref_split(&self, object: CapObject) {
        let mut release: NotificationRefRelease = NotificationRefRelease::new();
        release.note_object(object);
        self.release_notification_refs_split(&release);
    }

    /// Split: [`KernelState::release_notification_refs`]; the wake runs after rank 3 is released.
    pub(crate) fn release_notification_refs_split<const N: usize>(
        &self,
        release: &NotificationRefRelease<N>,
    ) {
        for (incarnation, count) in release.iter() {
            let outcome = self.with_ipc_split_mut(|ipc| {
                release_notification_refs_locked(ipc, incarnation, count)
            });
            if let NotificationReleaseOutcome::Destroyed {
                waiter: Some(record),
            } = outcome
            {
                let _ = self.wake_destroyed_notification_waiter_split(record);
            }
        }
    }
}
