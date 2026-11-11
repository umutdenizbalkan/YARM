#!/usr/bin/env python3
"""ENDPOINT-ROLLBACK1 production repair. Usage: er1_patch.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
T = "src/kernel/boot/spawn_ipc_cap_txn.rs"

patch(T, '''//! (memory rank 6). That work is real and stays. It is owed only by an endpoint that has been
//! published — one that acquired a waiter, a parked sender or an IRQ route. A provisional endpoint
//! that a failed spawn is giving back has none of those, and
//! [`remove_unpublished_endpoint_locked`] refuses rather than guesses: it checks all three, and
//! checks that the incarnation is still the one the token names.''',
'''//! (memory rank 6). That work is real and stays. It is owed only by an endpoint that has been
//! published — one that acquired a waiter or a parked sender. A provisional endpoint that a failed
//! spawn is giving back has neither, and [`remove_unpublished_endpoint_locked`] refuses rather
//! than guesses: it checks both, and checks that the incarnation is still the one the token names.
//!
//! An IRQ route is not one of these surfaces. It names a NOTIFICATION incarnation (IRQ-AUTH1), and
//! Notifications and Endpoints have independent slot indexes and generations, so no route can
//! refer to an endpoint (ENDPOINT-ROLLBACK1).''')

patch(T, '''/// The incarnation is still installed but has become externally visible — a blocked receiver,
    /// a parked sender, or an IRQ route names it. Removing it here would strand them; that is
    /// `destroy_endpoint`'s job and it needs ranks this body does not hold.''',
'''/// The incarnation is still installed but has become externally visible — a blocked receiver
    /// or a parked sender names it. Removing it here would strand them; that is
    /// `destroy_endpoint`'s job and it needs ranks this body does not hold.''')

patch(T, '''/// 3. **no sender is parked on it and no IRQ routes to it** — a parked sender may own a transfer
///    envelope that has to be settled (memory rank 6), and an IRQ route is an external reference.
///
/// A provisional endpoint from a failed spawn satisfies all three by construction: it was created
/// moments ago, no task has ever been given a capability to receive on it, and nothing routes to
/// it. If any check fails this returns without mutating, and the caller must use the live teardown.''',
'''/// 3. **no sender is parked on it** — a parked sender may own a transfer envelope that has to be
///    settled (memory rank 6).
///
/// IRQ routes are deliberately NOT consulted (ENDPOINT-ROLLBACK1). A route names a Notification
/// incarnation `{index, generation}` in the Notification namespace; an endpoint index in the same
/// numeric range is a different object. Comparing the two refused the rollback of any endpoint
/// whose slot number happened to equal a routed Notification's, and the failed spawn then kept
/// that endpoint forever. No replacement comparison of unrelated indexes is made either.
///
/// A provisional endpoint from a failed spawn satisfies all three by construction: it was created
/// moments ago and both of its capabilities were revoked (with their delegated children) before
/// this runs. If any check fails this returns without mutating, and the caller must use the live
/// teardown.''')

patch(T, '''    if ipc.endpoint_waiters[index].is_some()
        || ipc.endpoint_sender_waiters[index]
            .iter()
            .any(Option::is_some)
        // Pre-existing and preserved by IRQ-AUTH1: this compares Notification route targets with
        // an ENDPOINT index (recorded in the deferred-defects list). It can only refuse a
        // removal, never corrupt a route.
        || ipc
            .irq_routes
            .iter()
            .flatten()
            .any(|route| route.notification.index == index)
    {''', '''    // ENDPOINT-ROLLBACK1: only this endpoint's own publication surfaces. IRQ routes name
    // Notifications, never Endpoints, so they are not consulted.
    if ipc.endpoint_waiters[index].is_some()
        || ipc.endpoint_sender_waiters[index]
            .iter()
            .any(Option::is_some)
    {''')
print("patch applied")
