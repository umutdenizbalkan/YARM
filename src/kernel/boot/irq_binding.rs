// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! IRQ-AUTH1 — capability-authorized IRQ binding: one exclusive route per line, owned by the
//! exact IRQ capability that made it, published against the exact Notification incarnation.
//!
//! # Authorities
//!
//! * **Line authority** is a `CapObject::Irq { line }` capability with SIGNAL — the right every
//!   IRQ capability carries and the one `grant_driver_irq` delegates. Only the kernel mints one
//!   (`mint_irq_cap*`); no syscall can, and fork never inherits one.
//! * **Signal authority** is a Notification capability with SIGNAL.
//! * **Binding ownership** is the route's [`IrqBindingOwner`]: the cnode and the generation-bearing
//!   [`CapId`] of the IRQ capability the binding was made with.
//! * **The Notification's lifetime** stays NOTIF-LIFETIME1's alone. A route holds no reference.
//!
//! Binding line X to Notification N requires both authorities, resolved in the caller's own
//! cnode in one rank-4 snapshot. The line comes from the IRQ capability, never from a raw number.
//!
//! # The route lease
//!
//! `IpcSubsystem::irq_routes[X]` is `Some(IrqRoute { notification, owner })`. It lives exactly
//! while both hold:
//!
//! * the Notification incarnation `notification` is live. The final release clears every route
//!   naming it, in the critical section that destroys it (NOTIF-LIFETIME1);
//! * the owner capability is live. Every capability removal reports the IRQ lines it removed
//!   capabilities for, through the same removal report as the Notification references. After
//!   rank 4 is released, [`KernelState::revalidate_irq_routes`] clears each such line's route
//!   whose owner no longer resolves.
//!
//! So:
//!
//! * revoking the owner's capability, or a subtree containing it, or reaping its process,
//!   removes the binding;
//! * revoking a delegated copy or a sibling leaves it, because the owner still resolves;
//! * a binding is never transferred. A holder of the line's authority binds again once the old
//!   lease has ended.
//!
//! # One rank-3 decision
//!
//! [`bind_irq_route_locked`] checks the incarnation's presence and exact generation, and the
//! line's occupancy, and publishes, all in ONE IPC rank-3 critical section. No interval exists
//! between the generation check and the route write. An occupied line is never redirected:
//!
//! * the same incarnation is an idempotent success, which keeps the original owner;
//! * a different one is refused (`EndpointQueueFull`), unless the occupying owner was proved
//!   dead at rank 4, in which case exactly that record may be replaced.
//!
//! A bind whose owner capability is removed concurrently is caught by the rank-4 recheck after
//! publication, which retracts the exact record. Either the recheck runs after the removal and
//! sees it, or the removal's revalidation runs after the publication and clears it.

use super::{IpcSubsystem, KernelError, KernelState, MAX_NOTIFICATIONS};
use crate::arch::platform_constants::{FIRST_DEVICE_IRQ_LINE, MAX_IRQ_LINES};
use crate::kernel::boot::CapabilitySubsystem;
use crate::kernel::boot::notification_lifetime::NotificationIncarnation;
use crate::kernel::capabilities::{CNodeId, CapId, CapObject, CapRights};

/// The exact IRQ capability a binding was made with: its cnode, and its generation-bearing id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IrqBindingOwner {
    pub(crate) cnode: CNodeId,
    pub(crate) cap: CapId,
}

/// One line's binding: the exact Notification incarnation it delivers to, and its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IrqRoute {
    pub(crate) notification: NotificationIncarnation,
    pub(crate) owner: IrqBindingOwner,
}

/// What the rank-3 binding decision did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IrqBindOutcome {
    /// The route is published.
    Bound,
    /// The line was already bound to this exact incarnation; nothing changed.
    AlreadyBound,
    /// The line is bound to a different incarnation. Carries that route, so the caller can test
    /// its owner for staleness.
    Occupied(IrqRoute),
}

/// The line is in range and not kernel-owned on this port.
pub(crate) fn irq_line_is_bindable(line: u16) -> Result<(), KernelError> {
    if usize::from(line) >= MAX_IRQ_LINES {
        return Err(KernelError::WrongObject);
    }
    if line < FIRST_DEVICE_IRQ_LINE {
        return Err(KernelError::MissingRight);
    }
    Ok(())
}

/// Rank 3: THE binding decision. See the module contract.
///
/// `stale` is a route the caller proved dead at rank 4. It may be replaced only if the line
/// still holds exactly it.
pub(crate) fn bind_irq_route_locked(
    ipc: &mut IpcSubsystem,
    line: u16,
    route: IrqRoute,
    stale: Option<IrqRoute>,
) -> Result<IrqBindOutcome, KernelError> {
    let i = route.notification.index;
    if i >= MAX_NOTIFICATIONS || ipc.notifications[i].is_none() {
        return Err(KernelError::WrongObject);
    }
    if ipc.notification_generations[i] != route.notification.generation {
        return Err(KernelError::StaleCapability);
    }
    let slot = &mut ipc.irq_routes[usize::from(line)];
    match *slot {
        Some(current) if current.notification == route.notification => {
            Ok(IrqBindOutcome::AlreadyBound)
        }
        Some(current) if Some(current) != stale => Ok(IrqBindOutcome::Occupied(current)),
        _ => {
            *slot = Some(route);
            Ok(IrqBindOutcome::Bound)
        }
    }
}

/// Rank 3: the object a route delivers to — only its exact incarnation. A different generation in
/// the slot (destroyed and reused since the route was read) or an empty slot is `WrongObject`,
/// which delivery treats as a benign no-op.
pub(crate) fn routed_notification_locked(
    ipc: &mut IpcSubsystem,
    route: IrqRoute,
) -> Result<&mut crate::kernel::boot::NotificationObject, KernelError> {
    let i = route.notification.index;
    if ipc.notification_generations.get(i).copied() != Some(route.notification.generation) {
        return Err(KernelError::WrongObject);
    }
    ipc.notifications
        .get_mut(i)
        .and_then(|n| n.as_mut())
        .ok_or(KernelError::WrongObject)
}

/// Rank 3: clear `line`'s route only if it is exactly `route`. Returns whether it was cleared.
pub(crate) fn retract_irq_route_exact_locked(
    ipc: &mut IpcSubsystem,
    line: u16,
    route: IrqRoute,
) -> bool {
    let Some(slot) = ipc.irq_routes.get_mut(usize::from(line)) else {
        return false;
    };
    if *slot != Some(route) {
        return false;
    }
    *slot = None;
    true
}

/// Rank 4, read-only: whether `owner` still resolves to an IRQ capability for `line` with SIGNAL.
pub(crate) fn irq_owner_live_locked(
    capability: &CapabilitySubsystem,
    owner: IrqBindingOwner,
    line: u16,
) -> bool {
    capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == owner.cnode)
        .and_then(|space| super::kernel_ref(&space.cspace).get(owner.cap))
        .is_some_and(|cap| {
            cap.object == (CapObject::Irq { line }) && cap.has_right(CapRights::SIGNAL)
        })
}

/// The authenticated facts a bind needs, taken from one rank-4 snapshot of the caller's cnode.
#[derive(Clone, Copy, Debug)]
struct BindRequest {
    line: u16,
    route: IrqRoute,
}

/// Rank 4, read-only: resolve both capabilities in `cnode` and check their types and rights.
fn resolve_bind_request_locked(
    capability: &CapabilitySubsystem,
    cnode: CNodeId,
    irq_cap: CapId,
    notification_cap: CapId,
) -> Result<BindRequest, KernelError> {
    let space = capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == cnode)
        .ok_or(KernelError::TaskMissing)?;
    let cspace = super::kernel_ref(&space.cspace);
    let irq = cspace.get(irq_cap).ok_or(KernelError::InvalidCapability)?;
    let CapObject::Irq { line } = irq.object else {
        return Err(KernelError::WrongObject);
    };
    if !irq.has_right(CapRights::SIGNAL) {
        return Err(KernelError::MissingRight);
    }
    let notification = cspace
        .get(notification_cap)
        .ok_or(KernelError::InvalidCapability)?;
    if !notification.has_right(CapRights::SIGNAL) {
        return Err(KernelError::MissingRight);
    }
    let incarnation =
        NotificationIncarnation::of(notification.object).ok_or(KernelError::WrongObject)?;
    Ok(BindRequest {
        line,
        route: IrqRoute {
            notification: incarnation,
            owner: IrqBindingOwner {
                cnode,
                cap: irq_cap,
            },
        },
    })
}

impl KernelState {
    /// IRQ-AUTH1 — bind the line named by `irq_cap` to the Notification named by
    /// `notification_cap`, both resolved in the current task's cnode. See the module contract.
    pub fn bind_irq_notification(
        &mut self,
        irq_cap: CapId,
        notification_cap: CapId,
    ) -> Result<(), KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        let request = self.with_capability_state(|capability| {
            resolve_bind_request_locked(capability, cnode, irq_cap, notification_cap)
        })?;
        irq_line_is_bindable(request.line)?;
        #[cfg(test)]
        irq_bind_phase_hook::run_before_publish(self);
        let line = request.line;
        let route = request.route;
        let mut outcome =
            self.with_ipc_state_mut(|ipc| bind_irq_route_locked(ipc, line, route, None))?;
        if let IrqBindOutcome::Occupied(current) = outcome
            && !self.with_capability_state(|capability| {
                irq_owner_live_locked(capability, current.owner, line)
            })
        {
            crate::yarm_log!(
                "IRQ_ROUTE_STALE_RECLAIMED line={} notification={} generation={}",
                line,
                current.notification.index,
                current.notification.generation
            );
            outcome = self
                .with_ipc_state_mut(|ipc| bind_irq_route_locked(ipc, line, route, Some(current)))?;
        }
        match outcome {
            IrqBindOutcome::AlreadyBound => Ok(()),
            IrqBindOutcome::Occupied(_) => Err(KernelError::EndpointQueueFull),
            IrqBindOutcome::Bound => {
                // The owner may have been removed after the rank-4 snapshot. A removal that ran
                // before this recheck did not see the route, so it is retracted here; one that
                // runs after it revalidates the line itself.
                if self.with_capability_state(|capability| {
                    irq_owner_live_locked(capability, route.owner, line)
                }) {
                    crate::yarm_log!(
                        "IRQ_ROUTE_BOUND line={} notification={} generation={}",
                        line,
                        route.notification.index,
                        route.notification.generation
                    );
                    Ok(())
                } else {
                    let retracted = self
                        .with_ipc_state_mut(|ipc| retract_irq_route_exact_locked(ipc, line, route));
                    crate::yarm_log!(
                        "IRQ_ROUTE_RETRACTED line={} reason=owner_removed retracted={}",
                        line,
                        u8::from(retracted)
                    );
                    Err(KernelError::InvalidCapability)
                }
            }
        }
    }

    /// IRQ-AUTH1 — after a removal (rank 4 released): clear the route of every line in `lines`
    /// whose owner capability no longer resolves.
    pub(crate) fn revalidate_irq_routes(&mut self, lines: u64) {
        for line in (0..MAX_IRQ_LINES as u16).filter(|l| lines & (1u64 << l) != 0) {
            let Some(route) = self.with_ipc_state(|ipc| ipc.irq_routes[usize::from(line)]) else {
                continue;
            };
            if self.with_capability_state(|capability| {
                irq_owner_live_locked(capability, route.owner, line)
            }) {
                continue;
            }
            let cleared =
                self.with_ipc_state_mut(|ipc| retract_irq_route_exact_locked(ipc, line, route));
            crate::yarm_log!(
                "IRQ_ROUTE_RELEASED line={} notification={} generation={} cleared={}",
                line,
                route.notification.index,
                route.notification.generation,
                u8::from(cleared)
            );
        }
    }
}

impl crate::runtime::SharedKernel {
    /// Split: [`KernelState::revalidate_irq_routes`] through the rank-3 and rank-4 split seams.
    pub(crate) fn revalidate_irq_routes_split(&self, lines: u64) {
        for line in (0..MAX_IRQ_LINES as u16).filter(|l| lines & (1u64 << l) != 0) {
            let Some(route) = self.with_ipc_split_mut(|ipc| ipc.irq_routes[usize::from(line)])
            else {
                continue;
            };
            if self.with_capability_state_split_mut(|capability| {
                irq_owner_live_locked(capability, route.owner, line)
            }) {
                continue;
            }
            let cleared =
                self.with_ipc_split_mut(|ipc| retract_irq_route_exact_locked(ipc, line, route));
            crate::yarm_log!(
                "IRQ_ROUTE_RELEASED line={} notification={} generation={} cleared={}",
                line,
                route.notification.index,
                route.notification.generation,
                u8::from(cleared)
            );
        }
    }
}

/// The bit a removal report sets for an IRQ capability's line.
pub(crate) const fn irq_line_bit(line: u16) -> u64 {
    if (line as usize) < MAX_IRQ_LINES && line < 64 {
        1u64 << line
    } else {
        0
    }
}

/// Test-only: a route forced into a line by a test that simulates a defect (defence-in-depth
/// cases). Its owner names no capability that can ever exist.
#[cfg(test)]
pub(crate) fn forced_route_for_test(index: usize, generation: u64) -> IrqRoute {
    IrqRoute {
        notification: NotificationIncarnation { index, generation },
        owner: IrqBindingOwner {
            cnode: CNodeId(u64::MAX),
            cap: CapId::new(0, u64::MAX >> CapId::INDEX_BITS),
        },
    }
}

#[cfg(test)]
pub(crate) mod irq_bind_phase_hook {
    //! IRQ-AUTH1: a test-only phase handshake between the rank-4 capability snapshot and the
    //! rank-3 binding decision — the window a concurrent destroy, reuse or revoke lands in.
    extern crate std;
    use core::cell::RefCell;
    type Hook = alloc::boxed::Box<dyn FnMut(&mut crate::kernel::boot::KernelState)>;
    std::thread_local! {
        static BEFORE_PUBLISH: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm(hook: Hook) {
        BEFORE_PUBLISH.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_before_publish(state: &mut crate::kernel::boot::KernelState) {
        let hook = BEFORE_PUBLISH.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook(state);
        }
    }
}
