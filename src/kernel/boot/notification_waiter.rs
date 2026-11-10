// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! NOTIF-WAITER1 — the notification waiter: one blocking receiver per incarnation, published
//! without losing an event, and never onto an incarnation the receiver did not validate.
//!
//! # Contract
//!
//! `IpcSubsystem::notification_waiters[i]` names at most one blocked receiver of the incarnation
//! `{i, notification_generations[i]}`. Every decision about it is taken at IPC rank 3, in ONE
//! critical section that also sees the incarnation's generation and its event queue:
//!
//! * [`take_notification_event_locked`] — the immediate receive. The capability's generation is
//!   compared in the same acquisition as the take, so an event of a newer incarnation is never
//!   consumed by an older capability.
//! * [`publish_notification_waiter_locked`] — the blocking receive's publication, made AFTER the
//!   receiver is Blocked (so a signal that finds the record can always wake it). In order:
//!   a gone or recycled incarnation refuses; a queued event is consumed (the signal raced the
//!   block — taking it here is what makes the race lossless); a different receiver, or the same
//!   one without a strictly newer wait generation, refuses (`ReceiverAlreadyWaiting`) — unless
//!   the caller proved that exact record stale; otherwise the record is published, superseding
//!   only the same receiver's older block or a record proved stale.
//! * Staleness is decided at task rank 2, before re-entering rank 3: a record is live only while
//!   its `{tid, asid}` task is `Blocked(EndpointReceive)` with `blocked_recv_generation` equal
//!   to the record's wait generation. The generation only grows, so a record once stale is stale
//!   forever; and the rank-3 decision replaces it only if the slot still holds exactly that
//!   record. A record a lost wake left behind therefore never blocks the next receiver.
//! * [`retract_notification_waiter_exact_locked`] — remove a record only if it is exactly the
//!   given `{tid, asid, wait_generation}`.
//! * [`IpcSubsystem::clear_notification_waiters_for_identity`] — every teardown and timeout
//!   removal: exact `{tid, asid}`, never a bare TID.
//!
//! The signal side is unchanged: the IRQ delivery takes the record in the same acquisition that
//! queues the event, and wakes it through the exact-identity proof. With the publication above,
//! every interleaving of a signal and a blocking receive ends with the event either consumed by
//! the receiver or delivered to a published, wakeable waiter.
//!
//! A waiter holds no reference on the notification (NOTIF-LIFETIME1): the final release still
//! retires and wakes it.

use super::{EndpointWaiterRecord, IpcSubsystem, MAX_NOTIFICATIONS, ReceiverWaiterIdentity};
use crate::kernel::boot::notification_lifetime::NotificationIncarnation;
use crate::kernel::ipc::Message;

/// What the blocking receive's publication decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotificationPublishOutcome {
    /// The receiver is the incarnation's waiter.
    Published,
    /// An event was queued: it was consumed for the receiver instead of parking it.
    EventTaken(Message),
    /// Another receiver's record occupies the slot (or the same receiver's, without a newer wait
    /// generation). It carries that record so the caller can test it for staleness.
    ReceiverAlreadyWaiting(EndpointWaiterRecord),
    /// The slot holds no incarnation, or a different one than the capability names.
    IncarnationGone,
}

/// Rank 3: the immediate, generation-checked take. `Err(())` when the capability's incarnation
/// is gone or recycled.
pub(crate) fn take_notification_event_locked(
    ipc: &mut IpcSubsystem,
    incarnation: NotificationIncarnation,
) -> Result<Option<Message>, ()> {
    let i = incarnation.index;
    if i >= MAX_NOTIFICATIONS || ipc.notification_generations[i] != incarnation.generation {
        return Err(());
    }
    ipc.notifications[i]
        .as_mut()
        .map(|notification| notification.recv())
        .ok_or(())
}

/// Rank 3: the blocking receive's publication — see the module contract for the order.
///
/// `stale` is a record the caller proved stale at rank 2; it may be replaced only if the slot
/// still holds exactly it.
pub(crate) fn publish_notification_waiter_locked(
    ipc: &mut IpcSubsystem,
    incarnation: NotificationIncarnation,
    record: EndpointWaiterRecord,
    stale: Option<EndpointWaiterRecord>,
) -> NotificationPublishOutcome {
    let i = incarnation.index;
    if i >= MAX_NOTIFICATIONS
        || ipc.notification_generations[i] != incarnation.generation
        || ipc.notifications[i].is_none()
    {
        return NotificationPublishOutcome::IncarnationGone;
    }
    if let Some(msg) = ipc.notifications[i].as_mut().and_then(|n| n.recv()) {
        return NotificationPublishOutcome::EventTaken(msg);
    }
    if let Some(current) = ipc.notification_waiters[i]
        && Some(current) != stale
        && (current.receiver != record.receiver
            || current.wait_generation >= record.wait_generation)
    {
        return NotificationPublishOutcome::ReceiverAlreadyWaiting(current);
    }
    ipc.notification_waiters[i] = Some(record);
    NotificationPublishOutcome::Published
}

/// Rank 3: remove `record` from `incarnation`'s waiter slot only if it is exactly that record.
/// Returns whether it was removed.
pub(crate) fn retract_notification_waiter_exact_locked(
    ipc: &mut IpcSubsystem,
    incarnation: NotificationIncarnation,
    record: EndpointWaiterRecord,
) -> bool {
    let i = incarnation.index;
    if i >= MAX_NOTIFICATIONS
        || ipc.notification_generations[i] != incarnation.generation
        || ipc.notification_waiters[i] != Some(record)
    {
        return false;
    }
    ipc.notification_waiters[i] = None;
    true
}

impl IpcSubsystem {
    /// Rank 3: remove every notification waiter record naming exactly `identity`. A record of a
    /// different incarnation that happens to share the numeric TID is left alone.
    pub(crate) fn clear_notification_waiters_for_identity(
        &mut self,
        identity: ReceiverWaiterIdentity,
    ) {
        for waiter in self.notification_waiters.iter_mut() {
            if waiter.is_some_and(|w| w.receiver == identity) {
                *waiter = None;
            }
        }
    }

    /// Rank 3: whether any notification waiter record names exactly `identity`.
    pub(crate) fn any_notification_waiter_is(&self, identity: ReceiverWaiterIdentity) -> bool {
        self.notification_waiters
            .iter()
            .any(|w| w.is_some_and(|w| w.receiver == identity))
    }
}
